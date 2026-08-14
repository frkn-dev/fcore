use tracing::{error, Instrument};

use fcore::{
    http::{helpers as http, response::Instance},
    utils::get_uuid_last_octet_simple,
    Connection, ConnectionApiOperations, ConnectionBaseOperations, Distributor, Env, Error, Key,
    NodeStorageOperations, Status, Subscription, SubscriptionOperations, Tag,
};

use super::super::{
    super::subscription_audit,
    super::sync::{tasks::SyncOp, MemSync},
    param::KeyQueryParams,
    request::{ActivateKeyReq, KeyReq},
};
use super::connection::ensure_enabled_connections;

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

    if !code.is_valid(&secret) {
        return Ok(http::bad_request("Key is not valid"));
    }

    match db.get(code.as_str()).await {
        Some(key) => {
            if key.activated {
                return Ok(http::success_response(
                    "Key is valid and already activated".to_string(),
                    Some(key.id),
                    Instance::Key(key.clone()),
                ));
            }

            let instance = Instance::Key(key.clone());
            Ok(http::success_response(
                "Key is valid".to_string(),
                Some(key.id),
                instance,
            ))
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

    let days = req.days;
    let distributor = Distributor::new(distributor_str)
        .map_err(|_| Error::Custom("invalid distributor".to_string()))?;

    let db = memory.db.key();
    let key = Key::new(days, &distributor, &secret);

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

/// Post activate key
/// If subscription_id is not provided, a new subscription is created using the key's days
/// and default connections are created for the configured envs/tags.
pub async fn post_activate_key_handler<N, C, S>(
    req: ActivateKeyReq,
    trace_id_header: Option<String>,
    memory: MemSync<N, C, S>,
    wg_network: fcore::IpAddrMask,
    awg_network: fcore::IpAddrMask,
    awg_mobile_network: Option<fcore::IpAddrMask>,
    enabled_conns: Option<std::collections::HashMap<Env, Vec<Tag>>>,
    mrkting: Option<crate::config::MrktingConfig>,
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

    let sub_id = match req.subscription_id {
        Some(id) => id,
        None => {
            let sub_id = uuid::Uuid::new_v4();
            let ref_code = get_uuid_last_octet_simple(&sub_id);
            // expires_at stays empty on purpose: the shared add_days call
            // below grants the key's days exactly once. Setting it here too
            // would double them (days in expires_at + days from add_days).
            let expires_at = None;
            let sub = Subscription::new(sub_id, ref_code, expires_at, req.limit_bytes);

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
                    return Ok(http::bad_request(&format!("Failed to create subscription: {}", msg)));
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

    match SyncOp::add_days(
        &memory,
        &sub_id,
        key.days as i64,
    )
    .instrument(subscription_audit::transaction_span(
        "key_activate_handler",
        sub_id,
        Some(trace_id),
    ))
    .await
    {
        Ok(Status::Updated(_)) => {
            key.activate(&sub_id);
            if let Err(err) = key_db.activate(&key).await {
                return Ok(http::bad_request(&format!(
                    "Key activation failed: {}",
                    err
                )));
            }

            // Top up connections for every enabled (env, tag): existing ones
            // are kept, only missing protocols are created.
            ensure_enabled_connections(
                sub_id,
                &enabled_conns,
                &memory,
                &wg_network,
                &awg_network,
                &awg_mobile_network,
            )
            .await;

            if let Some(mrkting) = &mrkting {
                let client = reqwest::Client::new();
                let url = format!("{}/keys/activations", mrkting.endpoint.trim_end_matches('/'));
                let body = serde_json::json!({
                    "subscription_id": sub_id,
                    "key_code": req.code,
                    "days": key.days,
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
                            tracing::info!(
                                "mrkting key activation recorded for sub {}",
                                sub_id
                            );
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
