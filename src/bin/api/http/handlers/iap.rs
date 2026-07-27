use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use warp::http::StatusCode;
use warp::Reply;

use fcore::{
    http::{helpers as http, ResponseMessage},
    utils::get_uuid_last_octet_simple,
    Connection, ConnectionApiOperations, ConnectionBaseOperations, Env, NodeStorageOperations,
    Status, Subscription, SubscriptionOperations, SubscriptionStorageOperations, Tag,
};

use super::super::super::{
    config::MrktingConfig,
    iap::{validate_product_id, AppleIapClient},
    subscription_audit,
    sync::{tasks::SyncOp, MemSync},
};
use super::amnezia::{build_gateway_config_response, GatewayConfigParams};
use super::connection::create_connection_inner;

// ============================================================================
// DTOs
// ============================================================================

/// Request payload of `POST /v1/subscriptions` (App Store IAP binding).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GatewaySubscriptionsRequest {
    pub os_version: Option<String>,
    pub app_version: Option<String>,
    pub language: Option<String>,
    pub installation_uuid: Option<uuid::Uuid>,
    pub user_country_code: Option<String>,
    pub service_type: Option<String>,
    pub service_protocol: Option<String>,
    pub transaction_id: String,
    pub product_id: Option<String>,
}

// ============================================================================
// Helpers
// ============================================================================

/// JSON error response with an arbitrary status (same shape as http::helpers).
fn error_with_status(status: StatusCode, msg: &str) -> warp::reply::Response {
    let resp = ResponseMessage::<Option<uuid::Uuid>> {
        status: status.as_u16(),
        message: msg.to_string(),
        response: None,
    };
    warp::reply::with_status(warp::reply::json(&resp), status).into_response()
}

/// Makes sure the bound subscription exists. Returns `true` when it was
/// (re)created by this call, `false` when it already existed.
async fn ensure_subscription<N, C, S>(
    memory: &MemSync<N, C, S>,
    sub_id: &uuid::Uuid,
    expires_at: DateTime<Utc>,
) -> Result<bool, warp::reply::Response>
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
    Connection: From<C>,
{
    {
        let mem = memory.memory.read().await;
        if mem.subscriptions.find_by_id(sub_id).is_some() {
            return Ok(false);
        }
    }

    // The binding may exist from an earlier call while the in-memory state was
    // rebuilt (restart) — load the subscription back from PG if it is there.
    match memory.db.sub().find(sub_id).await {
        Ok(Some(sub)) => {
            let mut mem = memory.memory.write().await;
            mem.subscriptions.add(sub.into());
            return Ok(false);
        }
        Ok(None) => {}
        Err(e) => {
            return Err(error_with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Failed to load subscription: {e}"),
            ))
        }
    }

    let ref_code = get_uuid_last_octet_simple(sub_id);
    let sub = Subscription::new(*sub_id, ref_code, Some(expires_at), None);

    match SyncOp::add_sub(memory, sub).await {
        Ok(Status::Ok(_)) | Ok(Status::Updated(_)) | Ok(Status::AlreadyExist(_)) => Ok(true),
        Ok(other) => Err(error_with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to create subscription: {other:?}"),
        )),
        Err(e) => {
            // A concurrent call for the same transaction may have created it first.
            let mem = memory.memory.read().await;
            if mem.subscriptions.find_by_id(sub_id).is_some() {
                Ok(false)
            } else {
                Err(error_with_status(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Failed to create subscription: {e}"),
                ))
            }
        }
    }
}

/// Sets the exact expiration date received from Apple (memory + PG).
async fn set_subscription_expiration<N, C, S>(
    memory: &MemSync<N, C, S>,
    sub_id: &uuid::Uuid,
    expires_at: DateTime<Utc>,
) -> Result<(), warp::reply::Response>
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
    Connection: From<C>,
{
    let (was_inactive, old_expires_at) = {
        let mem = memory.memory.read().await;
        let sub = mem.subscriptions.find_by_id(sub_id);
        (
            sub.map(|s| !s.is_active()).unwrap_or(false),
            sub.and_then(|s| s.expires_at()),
        )
    };

    let (ref_code, parent_id, scope_env, premium_token) = {
        let mut mem = memory.memory.write().await;
        let sub = match mem.subscriptions.find_by_id_mut(sub_id) {
            Some(s) => s,
            None => return Err(http::not_found("Subscription not found").into_response()),
        };

        sub.set_expires_at(expires_at).map_err(|e| {
            error_with_status(StatusCode::BAD_REQUEST, &format!("Cannot extend: {e}"))
        })?;

        (
            sub.refer_code(),
            sub.parent_id(),
            sub.scope_env().cloned(),
            sub.premium_token().map(|t| t.to_string()),
        )
    };

    subscription_audit::log_days_change(
        "iap_extended",
        *sub_id,
        old_expires_at,
        Some(expires_at),
        None,
        "gateway_subscriptions_handler",
    );

    if let Err(e) = memory
        .db
        .sub()
        .update_subscription(
            *sub_id,
            expires_at,
            &ref_code,
            parent_id,
            scope_env.as_ref(),
            premium_token.as_deref(),
        )
        .await
    {
        return Err(error_with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to update subscription: {e}"),
        ));
    }

    if was_inactive {
        match SyncOp::restore_connections_by_subscription(memory, sub_id).await {
            Ok(restored) => {
                tracing::debug!(
                    "IAP: {} connections restored for {}",
                    restored.len(),
                    sub_id
                );
            }
            Err(e) => {
                tracing::error!("IAP: connection restore failed for {}: {:?}", sub_id, e);
            }
        }
    }

    Ok(())
}

/// Fire-and-forget report to the mrkting service; failures are only logged.
fn report_iap_bind(
    mrkting: MrktingConfig,
    original_transaction_id: String,
    product_id: String,
    environment: String,
    expires_at: DateTime<Utc>,
    installation_uuid: Option<uuid::Uuid>,
    subscription_id: uuid::Uuid,
) {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let url = format!("{}/iap/bind", mrkting.endpoint.trim_end_matches('/'));
        let body = serde_json::json!({
            "original_transaction_id": original_transaction_id,
            "product_id": product_id,
            "environment": environment,
            "expires_at": expires_at.to_rfc3339(),
            "installation_uuid": installation_uuid,
            "subscription_id": subscription_id,
        });

        match client
            .post(&url)
            .header("Authorization", format!("Bearer {}", mrkting.token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!("mrkting iap/bind recorded for sub {}", subscription_id);
            }
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                tracing::warn!("mrkting iap/bind call failed: {} {}", status, text);
            }
            Err(err) => {
                tracing::warn!(
                    "Failed to call mrkting iap/bind for sub {}: {}",
                    subscription_id,
                    err
                );
            }
        }
    });
}

// ============================================================================
// Handler
// ============================================================================

/// POST /v1/subscriptions — binds a verified App Store IAP transaction to a
/// subscription (creating it on first call) and returns a VPN config in the
/// same format as `gateway_config_handler`.
pub async fn gateway_subscriptions_handler<N, C, S>(
    req: GatewaySubscriptionsRequest,
    memory: MemSync<N, C, S>,
    iap: Option<Arc<AppleIapClient>>,
    wg_network: fcore::IpAddrMask,
    awg_network: fcore::IpAddrMask,
    enabled_conns: Option<HashMap<Env, Vec<Tag>>>,
    mrkting: Option<MrktingConfig>,
) -> Result<warp::reply::Response, warp::Rejection>
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
    Connection: From<C>,
{
    let iap = match iap {
        Some(client) => client,
        None => {
            return Ok(error_with_status(
                StatusCode::SERVICE_UNAVAILABLE,
                "App Store IAP is not configured",
            ))
        }
    };

    let transaction_id = req.transaction_id.trim();
    if transaction_id.is_empty() {
        return Ok(http::bad_request("transaction_id is required").into_response());
    }

    // 1. Fetch the signed transaction from the App Store Server API.
    let tx_info = match iap.api.get_transaction_info(transaction_id).await {
        Ok(info) => info,
        Err(e) => {
            return Ok(error_with_status(
                StatusCode::BAD_GATEWAY,
                &format!("App Store API error: {e:?}"),
            ))
        }
    };

    let signed_transaction = tx_info.signed_transaction_info.unwrap_or_default();
    if signed_transaction.is_empty() {
        return Ok(error_with_status(
            StatusCode::BAD_GATEWAY,
            "App Store returned an empty signedTransactionInfo",
        ));
    }

    // 2. Verify the JWS signature against the Apple root CA and decode it.
    //    The verifier also checks bundle_id and environment.
    let tx = match iap
        .verifier
        .verify_and_decode_signed_transaction(&signed_transaction)
    {
        Ok(tx) => tx,
        Err(e) => {
            return Ok(error_with_status(
                StatusCode::BAD_REQUEST,
                &format!("Transaction verification failed: {e}"),
            ))
        }
    };

    // 3. Product checks.
    let product_id = match validate_product_id(
        tx.product_id.as_deref(),
        req.product_id.as_deref(),
        &iap.config.allowed_products,
    ) {
        Ok(p) => p.to_string(),
        Err(msg) => return Ok(http::bad_request(&msg).into_response()),
    };

    if tx.revocation_date.is_some() {
        return Ok(http::bad_request("Transaction was revoked/refunded").into_response());
    }

    let original_transaction_id = match tx.original_transaction_id.as_deref() {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => {
            return Ok(
                http::bad_request("Transaction has no original_transaction_id").into_response(),
            )
        }
    };

    let expires_at = match tx.expires_date {
        Some(exp) => exp,
        None => {
            return Ok(http::bad_request("Transaction has no expires_date").into_response())
        }
    };

    if expires_at <= Utc::now() {
        return Ok(http::bad_request("Subscription already expired").into_response());
    }

    // 4. Find-or-create the subscription bound to this original transaction.
    //    The PG-level ON CONFLICT makes this idempotent under concurrent calls.
    let new_sub_id = uuid::Uuid::new_v4();
    let sub_id = match memory
        .db
        .iap()
        .bind_or_get(
            &original_transaction_id,
            &new_sub_id,
            &product_id,
            &iap.config.environment,
            req.installation_uuid,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            return Ok(error_with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Failed to bind transaction: {e}"),
            ))
        }
    };

    let created = match ensure_subscription(&memory, &sub_id, expires_at).await {
        Ok(created) => created,
        Err(resp) => return Ok(resp),
    };

    // 5. Extend the subscription to the exact date reported by Apple.
    if let Err(resp) = set_subscription_expiration(&memory, &sub_id, expires_at).await {
        return Ok(resp);
    }

    // 6. Default connections for a freshly created subscription.
    if created {
        if let Some(conns_map) = &enabled_conns {
            for (env, tags) in conns_map {
                for tag in tags {
                    if let Err(err) = create_connection_inner(
                        env,
                        *tag,
                        Some(sub_id),
                        None,
                        &memory,
                        &wg_network,
                        &awg_network,
                    )
                    .await
                    {
                        tracing::error!(
                            "Failed to create connection for sub {} env {:?} tag {:?}: {}",
                            sub_id,
                            env,
                            tag,
                            err
                        );
                    }
                }
            }
        }
    }

    // 7. Report to mrkting (fire-and-forget).
    if let Some(mrkting) = mrkting {
        report_iap_bind(
            mrkting,
            original_transaction_id,
            product_id,
            iap.config.environment.clone(),
            expires_at,
            req.installation_uuid,
            sub_id,
        );
    }

    // 8. VPN config in the gateway format.
    let params = GatewayConfigParams {
        service_protocol: req.service_protocol.as_deref().unwrap_or("vless"),
        service_type: req.service_type.as_deref().unwrap_or("amnezia-premium"),
        user_country_code: req.user_country_code.as_deref(),
        server_country_code: None,
        connection_id: None,
        public_key: None,
    };

    match build_gateway_config_response(&memory, &sub_id, &params).await {
        Ok(response) => Ok(warp::reply::json(&response).into_response()),
        Err(resp) => Ok(resp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gateway_subscriptions_request() {
        // Realistic payload as sent by the client inside the AGW envelope.
        let payload = serde_json::json!({
            "os_version": "iOS 18.2",
            "app_version": "4.8.1",
            "language": "ru",
            "installation_uuid": "3f6b2a1c-9d4e-4c5b-8a7f-2e1d0c9b8a76",
            "user_country_code": "RU",
            "service_type": "amnezia-premium",
            "service_protocol": "vless",
            "transaction_id": "2000000123456789",
            "product_id": "frkn_premium_1_month"
        });

        let req: GatewaySubscriptionsRequest = serde_json::from_value(payload).unwrap();
        assert_eq!(req.transaction_id, "2000000123456789");
        assert_eq!(req.product_id.as_deref(), Some("frkn_premium_1_month"));
        assert_eq!(req.service_protocol.as_deref(), Some("vless"));
        assert_eq!(
            req.installation_uuid,
            Some(uuid::Uuid::parse_str("3f6b2a1c-9d4e-4c5b-8a7f-2e1d0c9b8a76").unwrap())
        );
    }

    #[test]
    fn parses_minimal_payload() {
        let payload = serde_json::json!({
            "transaction_id": "2000000123456789"
        });

        let req: GatewaySubscriptionsRequest = serde_json::from_value(payload).unwrap();
        assert_eq!(req.transaction_id, "2000000123456789");
        assert!(req.product_id.is_none());
        assert!(req.installation_uuid.is_none());
        assert!(req.service_protocol.is_none());
    }

    #[test]
    fn rejects_missing_transaction_id() {
        let payload = serde_json::json!({
            "product_id": "frkn_premium_1_month"
        });

        assert!(serde_json::from_value::<GatewaySubscriptionsRequest>(payload).is_err());
    }
}
