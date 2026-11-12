//! Админ-ручки оповещений: история срабатываний и проверка канала.

use chrono::Utc;
use warp::{Rejection, Reply};

use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, NodeStorageOperations,
    Subscription, SubscriptionOperations,
};

use crate::sync::MemSync;

use super::admin::check_token;

fn json_status(code: u16, message: &str) -> warp::reply::Response {
    let status =
        warp::http::StatusCode::from_u16(code).unwrap_or(warp::http::StatusCode::BAD_REQUEST);
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({ "status": code, "message": message })),
        status,
    )
    .into_response()
}

/// GET /v1/admin/alerts?limit=50 — последние срабатывания правил.
pub async fn admin_alerts_handler<N, C, S>(
    limit: Option<i64>,
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
) -> Result<warp::reply::Response, Rejection>
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
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if !admin_enabled {
        return Ok(json_status(404, "not found"));
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(json_status(401, "unauthorized"));
    }

    let limit = limit.unwrap_or(50).clamp(1, 500);
    match memory.db.alerts().recent(limit).await {
        Ok(events) => Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 200,
                "response": { "events": events },
            })),
            warp::http::StatusCode::OK,
        )
        .into_response()),
        Err(e) => {
            tracing::error!("alerts: cannot read events: {e}");
            Ok(json_status(500, "internal"))
        }
    }
}

/// `POST /v1/admin/digest/send?kind=daily|weekly|monthly&dry_run=1` —
/// собрать сводку и отправить её (или только вернуть текст для проверки).
#[derive(Debug, serde::Deserialize)]
pub struct DigestQuery {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub dry_run: Option<bool>,
}

pub async fn admin_digest_handler<N, C, S>(
    query: DigestQuery,
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
    digest_cfg: crate::digest::DigestConfig,
    email: Option<crate::alerts::AlertEmailConfig>,
) -> Result<warp::reply::Response, Rejection>
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
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if !admin_enabled {
        return Ok(json_status(404, "not found"));
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(json_status(401, "unauthorized"));
    }
    let kind = crate::digest::DigestKind::parse(query.kind.as_deref().unwrap_or("daily"))
        .unwrap_or(crate::digest::DigestKind::Daily);
    let dry_run = query.dry_run.unwrap_or(false);
    let now = Utc::now();
    let (from, to) = digest_cfg.window_utc(kind, now);

    let digest = match memory.db.digest().collect(kind, from, to).await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("digest: collect failed: {e}");
            return Ok(json_status(500, "digest query failed"));
        }
    };
    let subject = digest.subject();
    let text = digest.render_text();

    if dry_run {
        return Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 200,
                "subject": subject,
                "text": text,
            })),
            warp::http::StatusCode::OK,
        )
        .into_response());
    }

    let Some(email) = email else {
        return Ok(json_status(400, "no [alerts.email] configured"));
    };
    match crate::digest::send_digest(&email, &subject, &text, &digest.render_html()).await {
        Ok(()) => Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 200,
                "delivered": true,
                "subject": subject,
            })),
            warp::http::StatusCode::OK,
        )
        .into_response()),
        Err(e) => {
            tracing::error!("digest: send failed: {e}");
            Ok(json_status(502, "smtp delivery failed"))
        }
    }
}

/// Тело `POST /v1/admin/alerts/send`: свободный текст от другого сервиса
/// (payment-gw шлёт так пустой сток ключей — молча терять оплаченный заказ
/// нельзя, а своего SMTP у гейтвея нет).
#[derive(Debug, serde::Deserialize)]
pub struct SendAlertRequest {
    #[serde(default)]
    pub subject: String,
    pub text: String,
}

const MAX_ALERT_TEXT: usize = 8000;

/// POST /v1/admin/alerts/send — отправить произвольное оповещение в
/// настроенные каналы. Ручка под `with_service_or_admin_auth`: её дёргают
/// наши же бекенды сервисным токеном (payment-gw сообщает о пустом стоке
/// ключей — своего SMTP у гейтвея нет).
pub async fn admin_alerts_send_handler(
    alerts: crate::alerts::AlertsConfig,
    body: SendAlertRequest,
) -> Result<warp::reply::Response, Rejection> {
    if !alerts.can_deliver() {
        return Ok(json_status(400, "alerts are not configured"));
    }
    let text = body.text.trim();
    if text.is_empty() {
        return Ok(json_status(400, "empty text"));
    }
    let text: String = text.chars().take(MAX_ALERT_TEXT).collect();
    let subject = body.subject.trim();
    let subject = if subject.is_empty() {
        crate::alerts::email_subject("ops")
    } else {
        subject.chars().take(200).collect()
    };

    let client = reqwest::Client::new();
    let report = crate::alerts::notify_report(&alerts, &client, &subject, "ops", &text).await;
    if !report.delivered() {
        tracing::error!("alerts: ops message delivery failed: {:?}", report);
    }

    let code = if report.delivered() { 200 } else { 502 };
    Ok(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({
            "status": code,
            "response": report.as_json(),
        })),
        warp::http::StatusCode::from_u16(code).unwrap_or(warp::http::StatusCode::BAD_GATEWAY),
    )
    .into_response())
}

/// POST /v1/admin/alerts/test — отправить тестовое сообщение во все
/// настроенные каналы (телеграм и почта) и вернуть результат по каждому.
pub async fn admin_alerts_test_handler(
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
    alerts: crate::alerts::AlertsConfig,
) -> Result<warp::reply::Response, Rejection> {
    if !admin_enabled {
        return Ok(json_status(404, "not found"));
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(json_status(401, "unauthorized"));
    }
    if !alerts.can_deliver() {
        return Ok(json_status(
            400,
            "alerts are not configured (alerts.enabled / telegram token+chat / email recipients)",
        ));
    }

    let text = format!(
        "🧪 FRKN alerts: тестовое сообщение\nвремя: {}",
        Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    );

    let client = reqwest::Client::new();
    let subject = crate::alerts::email_subject_state("test", "🧪", "проверка канала");
    let report = crate::alerts::notify_report(&alerts, &client, &subject, "test", &text).await;
    if !report.delivered() {
        tracing::error!("alerts: test delivery failed: {:?}", report);
    }

    let status = if report.delivered() {
        warp::http::StatusCode::OK
    } else {
        warp::http::StatusCode::BAD_GATEWAY
    };
    Ok(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({
            "status": status.as_u16(),
            "response": report.as_json(),
        })),
        status,
    )
    .into_response())
}
