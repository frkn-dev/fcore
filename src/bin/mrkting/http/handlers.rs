use chrono::Utc;
use std::sync::Arc;
use warp::Reply;

use super::request::{AccountRequest, RefCodeQuery};
use crate::{
    api_client::{ApiClient},
    config::{ServiceSettings, TrialConfig},
    crypto::EmailCipher,
    email::Mailer,
    postgres::PgContext,
};

#[derive(Clone)]
pub struct AppState {
    pub settings: ServiceSettings,
    pub pg: PgContext,
    pub cipher: Arc<EmailCipher>,
    pub mailer: Arc<Mailer>,
    pub api_client: Arc<ApiClient>,
}

pub async fn healthcheck_handler() -> Result<Box<dyn Reply + Send>, warp::Rejection> {
    Ok(Box::new(warp::reply::json(
        &serde_json::json!({"status": "ok"}),
    )))
}

pub async fn post_account_handler(
    state: AppState,
    req: AccountRequest,
    trace_id_header: Option<String>,
) -> Result<Box<dyn Reply + Send>, warp::Rejection> {
    let trace_id = trace_id_header
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .unwrap_or_else(uuid::Uuid::new_v4);

    let email = match req.email() {
        Some(e) if !e.is_empty() => e.to_lowercase(),
        _ => return Ok(bad_request("Email is required")),
    };

    let email_hmac = state.cipher.hmac(&email);

    if let Some(subscription_id) = req.subscription_id {
        let info = match state.api_client.get_subscription(subscription_id).await {
            Ok(i) => i,
            Err(e) => {
                tracing::error!("Failed to fetch subscription {}: {}", subscription_id, e);
                return Ok(bad_request(
                    &format!("Subscription {} not found", subscription_id)));
            }
        };

        let encrypted = match state.cipher.encrypt(&email) {
            Ok(c) => c,
            Err(e) => return Ok(internal_error(&format!("Encryption failed: {}", e))),
        };

        if let Err(e) = state
            .pg
            .emails()
            .insert(
                &encrypted,
                &email_hmac,
                false,
                req.referred_by.as_deref(),
                info.expires_at,
                Some(subscription_id),
                Some(&info.refer_code),
            )
            .await
        {
            tracing::error!("Failed to link email: {}", e);
            return Ok(internal_error("Failed to save email"));
        }

        return Ok(Box::new(warp::reply::json(
            &serde_json::json!({
                "subscription_id": subscription_id,
                "ref_code": info.refer_code,
            }),
        )));
    }

    let (days, limit_bytes) = if req.trial {
        (state.settings.trial.days, state.settings.trial.limit_bytes)
    } else {
        match (req.days, req.limit_bytes) {
            (Some(d), Some(l)) => (d, l),
            _ => {
                return Ok(bad_request(
                    "days and limit_bytes are required for non-trial accounts"))
            }
        }
    };

    if req.trial {
        match state.pg.emails().find_by_hmac(&email_hmac).await {
            Ok(Some(_)) => return Ok(bad_request("Trial already requested")),
            Err(e) => {
                tracing::error!("Failed to lookup email hmac: {}", e);
                return Ok(internal_error("Database error"));
            }
            _ => {}
        }
    }

    let encrypted = match state.cipher.encrypt(&email) {
        Ok(c) => c,
        Err(e) => return Ok(internal_error(&format!("Encryption failed: {}", e))),
    };

    let referred_by = req.referred_by.clone().unwrap_or_else(|| "WEB".to_string());
    let expires_at = Some(Utc::now() + chrono::Duration::days(days));

    let row_id = match state
        .pg
        .emails()
        .insert(
            &encrypted,
            &email_hmac,
            req.trial,
            Some(&referred_by),
            expires_at,
            None,
            None,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("Failed to insert email: {}", e);
            return Ok(internal_error("Failed to save email"));
        }
    };

    let info = match state
        .api_client
        .create_subscription(days, limit_bytes, Some(trace_id))
        .await
    {
        Ok(i) => i,
        Err(e) => {
            tracing::error!("Failed to create subscription: {}", e);
            return Ok(internal_error("Failed to create subscription"));
        }
    };

    if let Err(e) = state
        .pg
        .emails()
        .update_subscription_id_and_ref_code(row_id, info.id, &info.refer_code)
        .await
    {
        tracing::error!("Failed to update email record {}: {}", row_id, e);
    }

    create_connections(
        &state,
        info.id,
        &state.settings.trial,
        Some(trace_id),
    )
    .await;

    state
        .mailer
        .send_welcome_email(email, info.id, req.language);

    Ok(Box::new(warp::reply::json(
        &serde_json::json!({
            "subscription_id": info.id,
            "ref_code": info.refer_code,
            "message": "Account created. Check email",
        }),
    )))
}

async fn create_connections(
    state: &AppState,
    subscription_id: uuid::Uuid,
    trial: &TrialConfig,
    trace_id: Option<uuid::Uuid>,
) {
    for env in &trial.enabled_envs {
        for tag in &trial.enabled_tags {
            if let Err(e) = state
                .api_client
                .create_connection(env, subscription_id, tag, trace_id)
                .await
            {
                tracing::error!(
                    "Failed to create connection for sub {} env {:?} tag {:?}: {}",
                    subscription_id,
                    env,
                    tag,
                    e
                );
            }
        }
    }
}

pub async fn get_referral_stats_handler(
    state: AppState,
    query: RefCodeQuery,
) -> Result<Box<dyn Reply + Send>, warp::Rejection> {
    let count: i64 = match state.pg.emails().count_invited_by(&query.code).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to count referrals: {}", e);
            return Ok(internal_error("Database error"));
        }
    };

    Ok(Box::new(warp::reply::json(
        &serde_json::json!({
            "code": query.code,
            "invited_count": count,
        }),
    )))
}

pub async fn get_validate_ref_code_handler(
    state: AppState,
    query: RefCodeQuery,
) -> Result<Box<dyn Reply + Send>, warp::Rejection> {
    let valid = match state.pg.emails().get_by_ref_code(&query.code).await {
        Ok(Some(row)) => {
            if row.subscription_id.is_none() {
                false
            } else {
                match state.api_client.get_subscription(row.subscription_id.unwrap()).await {
                    Ok(info) => info
                        .expires_at
                        .map(|e| e > Utc::now())
                        .unwrap_or(false),
                    Err(e) => {
                        tracing::warn!("Failed to verify subscription for ref code: {}", e);
                        false
                    }
                }
            }
        }
        Ok(None) => false,
        Err(e) => {
            tracing::error!("Failed to lookup ref code: {}", e);
            false
        }
    };

    Ok(Box::new(warp::reply::json(
        &serde_json::json!({ "valid": valid }),
    )))
}

fn bad_request(msg: &str) -> Box<dyn Reply + Send> {
    Box::new(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"status": 400, "message": msg})),
        warp::http::StatusCode::BAD_REQUEST,
    ))
}

fn internal_error(msg: &str) -> Box<dyn Reply + Send> {
    Box::new(warp::reply::with_status(
        warp::reply::json(
            &serde_json::json!({"status": 500, "message": msg})),
        warp::http::StatusCode::INTERNAL_SERVER_ERROR,
    ))
}
