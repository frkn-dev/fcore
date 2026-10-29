//! Админ-ручки истории использования: выходы и входы.

use chrono::{DateTime, NaiveDate, Utc};
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

#[derive(serde::Deserialize, Debug, Default)]
pub struct UsageQuery {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub group_by: Option<String>,
}

fn parse_day(raw: Option<&str>, fallback: NaiveDate) -> NaiveDate {
    raw.and_then(|v| {
        DateTime::parse_from_rfc3339(v)
            .ok()
            .map(|d| d.with_timezone(&Utc).date_naive())
            .or_else(|| NaiveDate::parse_from_str(v, "%Y-%m-%d").ok())
    })
    .unwrap_or(fallback)
}

/// GET /v1/admin/usage/exit — кто через какой выход ходил.
pub async fn admin_exit_usage_handler<N, C, S>(
    query: UsageQuery,
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

    let today = Utc::now().date_naive();
    let from = parse_day(query.from.as_deref(), today - chrono::Duration::days(7));
    let to = parse_day(query.to.as_deref(), today);
    let group_by = query.group_by.as_deref().unwrap_or("ip");

    match memory.db.usage().aggregate_exit(from, to, group_by).await {
        Ok(groups) => Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 200,
                "response": {
                    "kind": "exit",
                    "from": from,
                    "to": to,
                    "group_by": group_by,
                    "groups": groups,
                }
            })),
            warp::http::StatusCode::OK,
        )
        .into_response()),
        Err(err) => {
            tracing::error!("exit usage aggregate failed: {err}");
            Ok(json_status(500, "usage_aggregate_failed"))
        }
    }
}

/// GET /v1/admin/usage/entry — откуда подключались (данные от агентов нод).
pub async fn admin_entry_usage_handler<N, C, S>(
    query: UsageQuery,
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

    let today = Utc::now().date_naive();
    let from = parse_day(query.from.as_deref(), today - chrono::Duration::days(7));
    let to = parse_day(query.to.as_deref(), today);
    let group_by = query.group_by.as_deref().unwrap_or("asn");

    match memory.db.usage().aggregate_entry(from, to, group_by).await {
        Ok(groups) => Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 200,
                "response": {
                    "kind": "entry",
                    "from": from,
                    "to": to,
                    "group_by": group_by,
                    "groups": groups,
                    "note": "входы появляются после обновления агентов нод",
                }
            })),
            warp::http::StatusCode::OK,
        )
        .into_response()),
        Err(err) => {
            tracing::error!("entry usage aggregate failed: {err}");
            Ok(json_status(500, "usage_aggregate_failed"))
        }
    }
}
