use std::str::FromStr;
use tracing::{error, Instrument};

use fcore::{
    http::{helpers as http, response::Instance},
    utils::get_uuid_last_octet_simple,
    Connection, ConnectionApiOperations, ConnectionBaseOperations, Distributor, Error, Key,
    KeyKind, NodeStorageOperations, PlanKind, Product, Status, Subscription,
    SubscriptionOperations,
};

use super::super::{
    super::subscription_audit,
    super::sync::{tasks::SyncOp, MemSync},
    param::KeyQueryParams,
    request::{ActivateKeyReq, KeyReq},
};
use super::connection::ensure_enabled_connections;

/// Serializes a key for API responses, adding the traffic_gib view of
/// traffic_bytes (null for standard keys) next to the raw key fields.
fn key_instance_json(key: &Key) -> serde_json::Value {
    let mut value = serde_json::to_value(key).unwrap_or_default();
    if let Some(obj) = value.as_object_mut() {
        let traffic_gib = key.traffic_bytes.map(|b| b / (1024 * 1024 * 1024));
        obj.insert("traffic_gib".to_string(), serde_json::json!(traffic_gib));
    }
    value
}

/// Same envelope as http::success_response, but with the key instance
/// carrying the extra traffic_gib field.
fn key_success_response(msg: String, key: &Key) -> warp::reply::WithStatus<warp::reply::Json> {
    let body = serde_json::json!({
        "status": warp::http::StatusCode::OK.as_u16(),
        "message": msg,
        "response": { "id": key.id, "instance": { "Key": key_instance_json(key) } },
    });
    warp::reply::with_status(warp::reply::json(&body), warp::http::StatusCode::OK)
}

/// Get specific & validate key handler
pub async fn get_key_validate_handler<N, C, S>(
    params: KeyQueryParams,
    memory: MemSync<N, C, S>,
    secret: Vec<u8>,
) -> Result<impl warp::Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + 'static,
{
    let code = params.key;
    let db = memory.db.key();

    if code.validate_payload(&secret).is_err() {
        return Ok(http::bad_request("Key is not valid"));
    }

    match db.get(code.as_str()).await {
        Some(key) => {
            if key.activated {
                return Ok(key_success_response(
                    "Key is valid and already activated".to_string(),
                    &key,
                ));
            }

            Ok(key_success_response("Key is valid".to_string(), &key))
        }
        None => Ok(http::not_found("Key is not found")),
    }
}

/// Post key handler
pub async fn post_key_handler<N, C, S>(
    req: KeyReq,
    memory: MemSync<N, C, S>,
    secret: Vec<u8>,
) -> Result<impl warp::Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + 'static,
{
    const DEFAULT_DISTRIBUTOR: &str = "FRKN";
    let distributor_str = req.distributor.as_deref().unwrap_or(DEFAULT_DISTRIBUTOR);

    let kind = match req.kind.as_deref().map(KeyKind::from_str) {
        Some(Ok(kind)) => kind,
        Some(Err(_)) => return Ok(http::bad_request("Unknown key kind")),
        None => KeyKind::Standard,
    };

    let distributor = Distributor::new(distributor_str)
        .map_err(|_| Error::Custom("invalid distributor".to_string()))?;

    let product = match req.product.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => match Product::from_str(s) {
            Ok(product) => product,
            Err(_) => return Ok(http::bad_request("Unknown product")),
        },
        _ => Product::Standard,
    };

    // Products other than the default `standard` are unlimited-only: they
    // are never sold as traffic (lite) keys.
    if !product.is_standard() && kind != KeyKind::Standard {
        return Ok(http::bad_request(
            "Tariff products other than 'standard' are only available for standard (unlimited) keys",
        ));
    }

    let key = match kind {
        KeyKind::Standard => Key::new(req.days, &distributor, &secret).with_product(product),
        KeyKind::Lite => {
            let traffic_gib = match req.traffic_gib {
                Some(gib) if gib > 0 => gib,
                _ => return Ok(http::bad_request("traffic_gib must be greater than 0")),
            };
            Key::new_lite(traffic_gib, &distributor, &secret)
        }
    };

    let db = memory.db.key();

    match db.insert(&key).await {
        Ok(_) => {
            let msg = format!("Key {} is created", key.id);
            Ok(http::success_response(
                msg,
                Some(key.id),
                Instance::Key(key),
            ))
        }
        Err(e) => {
            error!("Failed to insert key: {:?}", e);
            Ok(http::bad_request("Key create error"))
        }
    }
}

/// Basic sanity check for the lite activation email.
fn is_valid_activation_email(email: Option<&str>) -> bool {
    match email {
        Some(e) => e.len() > 3 && e.contains('@'),
        None => false,
    }
}

/// Post activate key
/// If subscription_id is not provided, a new subscription is created using the key's days
/// and default connections are created for the configured envs/tags.
/// Lite keys grant traffic instead of days: they create a subscription with
/// expires_at NULL and connections from lite_enabled_conns (falling back to
/// enabled_conns). Email is optional: when present, it is bound to the
/// subscription via mrkting.
/// Which activation a caller wants.
///
/// Both flows create the subscription the same way; they differ in how access
/// is granted. Keeping them behind two endpoints means the cutover is a change
/// on the site (a URL), not a config flag and not a restart — and rolling back
/// is pointing the site at the old URL again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationFlow {
    /// Product catalogue + `subscription.expires_at` (the model we are
    /// draining). Never writes grants.
    Legacy,
    /// Shards and grants: the api picks an env, writes `subscription_access`,
    /// and there is no fallback — a payer without a live shard is quarantined.
    Peers,
}

pub async fn post_activate_key_handler<N, C, S>(
    req: ActivateKeyReq,
    trace_id_header: Option<String>,
    memory: MemSync<N, C, S>,
    wg_network: fcore::IpAddrMask,
    awg_network: fcore::IpAddrMask,
    awg_mobile_network: Option<fcore::IpAddrMask>,
    resolver: crate::entitlements::ConnResolver,
    mrkting: Option<crate::config::MrktingConfig>,
    flow: ActivationFlow,
) -> Result<impl warp::Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + From<Subscription> + PartialEq,
    Connection: From<C>,
{
    let trace_id = subscription_audit::trace_id_from_header(trace_id_header);
    let key_db = memory.db.key();

    let mut key = match key_db.get(&req.code).await {
        Some(k) => k,
        None => return Ok(http::not_found("Key not found")),
    };

    if key.activated {
        return Ok(http::bad_request("Key already activated"));
    }

    // Which shared connections this key buys. Resolved before anything is
    // written: a product whose entitlements are misconfigured must fail the
    // activation without consuming the key.
    let key_plan_kind = match key.kind {
        KeyKind::Standard => PlanKind::Standard,
        KeyKind::Lite => PlanKind::Lite,
    };
    let catalog_map = match resolver.resolve(key_plan_kind, key.product) {
        Ok(map) => map,
        Err(err) => {
            error!("Cannot activate key {}: {}", key.id, err);
            return Ok(http::internal_error(&format!(
                "Activation is not configured: {}",
                err
            )));
        }
    };

    // Cross-product renewal is rejected before the expiry, the key and the
    // connections are touched: a subscription keeps the product it was
    // activated with. Another product is a new independent subscription,
    // i.e. an activation without subscription_id.
    if let Some(existing_id) = req.subscription_id {
        match memory.db.sub().find(&existing_id).await {
            Ok(Some(existing)) if existing.product != key.product => {
                return Ok(http::conflict(&format!(
                    "Key product '{}' does not match subscription product '{}'. \
                     Renew only with the same product, or activate the key as a new subscription.",
                    key.product, existing.product
                )));
            }
            Ok(_) => {}
            Err(err) => {
                error!(
                    "Failed to load subscription {} for a product check: {}",
                    existing_id, err
                );
                return Ok(http::internal_error("Failed to load subscription"));
            }
        }
    }

    let sub_id = match req.subscription_id {
        Some(id) => id,
        None => {
            let sub_id = uuid::Uuid::new_v4();
            let ref_code = get_uuid_last_octet_simple(&sub_id);
            let sub = match key.kind {
                // The traffic limit comes from the key; expires_at stays
                // NULL — a lite sub is active while it has traffic left.
                KeyKind::Lite => {
                    Subscription::new_lite(sub_id, ref_code, key.traffic_bytes.unwrap_or(0))
                }
                // expires_at stays empty on purpose: the shared add_days call
                // below grants the key's days exactly once. Setting it here too
                // would double them (days in expires_at + days from add_days).
                // The tariff product comes from the key, never from the
                // request: activation is the only place it is ever set.
                KeyKind::Standard => Subscription::new(sub_id, ref_code, None, req.limit_bytes)
                    .with_product(key.product),
            };

            subscription_audit::log_transaction_start(sub_id, Some(key.days as i64));

            match SyncOp::add_sub(&memory, sub.clone())
                .instrument(subscription_audit::transaction_span(
                    "key_activate_create_sub",
                    sub_id,
                    Some(trace_id),
                ))
                .await
            {
                Ok(Status::Ok(_)) | Ok(Status::Updated(_)) => sub_id,
                Ok(Status::AlreadyExist(_)) => sub_id,
                Ok(Status::NotFound(_)) => {
                    return Ok(http::not_found("Subscription not found"));
                }
                Ok(Status::BadRequest(_, msg)) => {
                    return Ok(http::bad_request(&format!(
                        "Failed to create subscription: {}",
                        msg
                    )));
                }
                Err(err) => {
                    return Ok(http::bad_request(&format!(
                        "Failed to create subscription: {}",
                        err
                    )));
                }
                _ => return Ok(http::not_modified("")),
            }
        }
    };

    subscription_audit::log_transaction_start(sub_id, Some(key.days as i64));

    let add_days_status = match key.kind {
        // Lite subs get no days: the limit was set at creation for a new sub,
        // and added here for an existing one (top-up with a lite key).
        KeyKind::Lite => match req.subscription_id {
            None => Ok(Status::Updated(sub_id)),
            Some(_) => {
                SyncOp::add_limit_bytes(&memory, &sub_id, key.traffic_bytes.unwrap_or(0), &key.id)
                    .instrument(subscription_audit::transaction_span(
                        "key_activate_lite_topup",
                        sub_id,
                        Some(trace_id),
                    ))
                    .await
            }
        },
        KeyKind::Standard => {
            SyncOp::add_days(&memory, &sub_id, key.days as i64)
                .instrument(subscription_audit::transaction_span(
                    "key_activate_handler",
                    sub_id,
                    Some(trace_id),
                ))
                .await
        }
    };

    // Shard assignment happens before the key is consumed: a paid key must
    // not burn because the api has nowhere to put the user. A renewal keeps
    // the shard it already has (sticky by plan) and never fails here.
    // Assignment kicks in only once the plan declares shards in
    // `env_capacity`: before that the feature is not rolled out and activation
    // keeps the legacy path. Once declared, a plan with no live shard is a
    // hard stop (the payer is quarantined instead of silently unserved).
    let plan = key.product.to_string();
    let declared_shards = memory
        .db
        .peers()
        .plan_shards(&plan)
        .await
        .unwrap_or_default();
    if flow == ActivationFlow::Peers && declared_shards.is_empty() {
        // Not a single shard is declared for this plan: serving is impossible
        // and the payer must not be charged for our planning gap.
        let _ = memory
            .db
            .peers()
            .record_activation_failure(
                key.id,
                &key.code,
                &plan,
                &plan,
                "plan_not_configured",
                Some(sub_id),
            )
            .await;
        error!(
            "activation blocked: plan '{}' has no shards in env_capacity (key {:?})",
            plan, key.code
        );
        return Ok(warp::reply::with_status(
            warp::reply::json(&fcore::http::ResponseMessage::<Option<serde_json::Value>> {
                status: 503,
                message: "Сейчас нет доступных серверов. Напишите в поддержку — ключ не потрачен."
                    .to_string(),
                // Stable code the app maps to «нет серверов, ключ не потрачен».
                response: Some(serde_json::json!({ "error": "plan_not_configured" })),
            }),
            warp::http::StatusCode::SERVICE_UNAVAILABLE,
        ));
    }

    if flow == ActivationFlow::Peers {
        let protos: Vec<fcore::Tag> = catalog_map
            .as_ref()
            .map(|map| {
                map.values()
                    .flatten()
                    .copied()
                    .filter(|tag| !tag.is_peer())
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default();

        if !protos.is_empty() {
            let expires_at = memory
                .db
                .sub()
                .find(&sub_id)
                .await
                .ok()
                .flatten()
                .and_then(|s| s.expires_at());
            // TODO: thread `peers.subscriptions_per_node` from settings; until
            // then the documented default is used here.
            let shards = crate::http::handlers::peers::live_shards(&memory, &plan, 100).await;
            match memory
                .db
                .peers()
                .assign_grant(
                    sub_id,
                    &plan,
                    &protos,
                    "payment",
                    expires_at,
                    key.traffic_bytes,
                    &shards,
                )
                .await
            {
                Ok(Some((row, used, limit, overflow))) => {
                    if overflow {
                        error!(
                            "activation: shards of plan '{}' are over capacity ({}/{}), add a node",
                            plan, used, limit
                        );
                    }
                    // Write-through: the grant is in Postgres, memory gets the
                    // same row so `POST /v1/peers` of this very subscription
                    // sees its rights without a restart.
                    memory
                        .memory
                        .write()
                        .await
                        .peers_state
                        .access
                        .insert(sub_id, row.clone());
                    tracing::info!(
                        "activation: subscription {} assigned to {} ({}/{})",
                        sub_id,
                        row.env,
                        used,
                        limit
                    );
                }
                Ok(None) => {
                    let _ = memory
                        .db
                        .peers()
                        .record_activation_failure(
                            key.id,
                            &key.code,
                            &plan,
                            &plan,
                            "no_live_shard",
                            Some(sub_id),
                        )
                        .await;
                    error!(
                        "activation blocked: no live shard for plan '{}' (key {:?})",
                        plan, key.code
                    );
                    return Ok(warp::reply::with_status(
                        warp::reply::json(&fcore::http::ResponseMessage::<Option<serde_json::Value>> {
                            status: 503,
                            message: "Сейчас нет доступных серверов. Напишите в поддержку — ключ не потрачен.".to_string(),
                            response: Some(serde_json::json!({ "error": "no_live_shard" })),
                        }),
                        warp::http::StatusCode::SERVICE_UNAVAILABLE,
                    ));
                }
                Err(e) => {
                    error!("activation: cannot assign shard for {}: {}", sub_id, e);
                    return Ok(warp::reply::with_status(
                        warp::reply::json(&fcore::http::ResponseMessage::<Option<serde_json::Value>> {
                            status: 503,
                            message:
                                "Не удалось выдать доступ. Напишите в поддержку — ключ не потрачен."
                                    .to_string(),
                            response: Some(serde_json::json!({ "error": "assign_failed" })),
                        }),
                        warp::http::StatusCode::SERVICE_UNAVAILABLE,
                    ));
                }
            }
        }
    }

    match add_days_status {
        Ok(Status::Updated(_)) => {
            key.activate(&sub_id);
            if let Err(err) = key_db.activate(&key).await {
                return Ok(http::bad_request(&format!(
                    "Key activation failed: {}",
                    err
                )));
            }

            // Grants win over the catalogue for this subscription: once the
            // payer's rights are stored, the product catalogue stops deciding
            // what to create. A subscription without grants keeps the old
            // behaviour unchanged.
            let conns_map =
                match crate::access::conns_map_for(&memory.db, sub_id, catalog_map.clone()).await {
                    Ok(map) => map,
                    Err(e) => {
                        error!(
                            "Cannot read grants for {}: {} — using the catalogue",
                            sub_id, e
                        );
                        catalog_map
                    }
                };

            let _ = memory.db.peers().resolve_activation_failure(key.id).await;

            match key.kind {
                // Lite subs get connections from lite_enabled_conns; the
                // fallback to the full enabled_conns is inside the resolver.
                KeyKind::Lite => {
                    ensure_enabled_connections(
                        sub_id,
                        &conns_map,
                        &memory,
                        &wg_network,
                        &awg_network,
                        &awg_mobile_network,
                    )
                    .await;
                }
                // Top up connections for every entitled (env, tag): existing
                // ones are kept, only missing protocols are created. For a
                // new product these are its six-product entitlements, for a
                // legacy key the historical enabled_conns.
                KeyKind::Standard => {
                    ensure_enabled_connections(
                        sub_id,
                        &conns_map,
                        &memory,
                        &wg_network,
                        &awg_network,
                        &awg_mobile_network,
                    )
                    .await;
                }
            }

            if let Some(mrkting) = &mrkting {
                let client = reqwest::Client::new();
                let url = format!(
                    "{}/keys/activations",
                    mrkting.endpoint.trim_end_matches('/')
                );
                let body = serde_json::json!({
                    "subscription_id": sub_id,
                    "key_code": req.code,
                    "days": key.days,
                    "kind": key.kind.to_string(),
                    "traffic_bytes": key.traffic_bytes,
                });

                match client
                    .post(&url)
                    .header("Authorization", format!("Bearer {}", mrkting.token))
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(resp) => {
                        if !resp.status().is_success() {
                            let status = resp.status();
                            let text = resp.text().await.unwrap_or_default();
                            tracing::warn!(
                                "mrkting key activation call failed: {} {}",
                                status,
                                text
                            );
                        } else {
                            tracing::info!("mrkting key activation recorded for sub {}", sub_id);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            "Failed to call mrkting key activation for sub {}: {}",
                            sub_id,
                            err
                        );
                    }
                }
            }

            // Bind the email to the lite subscription via mrkting when one
            // was provided. A failure here must not roll back the activation.
            if key.kind == KeyKind::Lite {
                let email = req
                    .email
                    .as_deref()
                    .filter(|e| is_valid_activation_email(Some(e)));
                match (&mrkting, email) {
                    (Some(mrkting), Some(email)) => {
                        let client = reqwest::Client::new();
                        let url = format!("{}/account", mrkting.endpoint.trim_end_matches('/'));
                        let body = serde_json::json!({
                            "subscription_id": sub_id,
                            "email": email,
                            "marketing_emails": false,
                            "system_notifications": true,
                        });

                        match client
                            .post(&url)
                            .header("Authorization", format!("Bearer {}", mrkting.token))
                            .header("Content-Type", "application/json")
                            .json(&body)
                            .send()
                            .await
                        {
                            Ok(resp) => {
                                if !resp.status().is_success() {
                                    let status = resp.status();
                                    let text = resp.text().await.unwrap_or_default();
                                    tracing::error!(
                                        "mrkting account bind failed for lite sub {}: {} {}",
                                        sub_id,
                                        status,
                                        text
                                    );
                                } else {
                                    tracing::info!("mrkting account bound for lite sub {}", sub_id);
                                }
                            }
                            Err(err) => {
                                tracing::error!(
                                    "Failed to call mrkting account bind for lite sub {}: {}",
                                    sub_id,
                                    err
                                );
                            }
                        }
                    }
                    (None, Some(_)) => {
                        tracing::error!(
                            "mrkting is not configured: email not bound for lite sub {}",
                            sub_id
                        );
                    }
                    _ => {}
                }
            }

            Ok(http::success_response(
                format!("Key {} activated", key.id),
                Some(key.id),
                Instance::Key(key),
            ))
        }
        Ok(Status::NotFound(_)) => Ok(http::not_found("Subscription not found")),
        Err(err) => Ok(http::bad_request(&format!("Failed to add days: {}", err))),
        _ => Ok(http::not_modified("")),
    }
}
