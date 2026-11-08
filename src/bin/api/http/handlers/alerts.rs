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

/// POST /v1/admin/alerts/test — отправить тестовое сообщение во все каналы,
/// которые выбраны в `[alerts] channel`, и вернуть результат по каждому.
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
            "alerts are not configured (alerts.enabled / channel / telegram token+chat / email recipients)",
        ));
    }

    let channel = alerts.channel.trim().to_ascii_lowercase();
    let text = format!(
        "🧪 FRKN alerts: тестовое сообщение\nканал: {}\nвремя: {}",
        if channel.is_empty() { "telegram" } else { &channel },
        Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    );

    let client = reqwest::Client::new();
    let report = crate::alerts::notify_report(&alerts, &client, &text).await;
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
