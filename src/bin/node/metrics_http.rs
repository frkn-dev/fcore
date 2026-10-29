//! HTTP ноды: /metrics (Prometheus), /healthz (публичный, минимум) и
//! /healthcheck (служебная информация под токеном).

use std::sync::Arc;

use warp::{Filter, Rejection, Reply};

use crate::node::Node;
use fcore::BaseConnection as Connection;

pub fn routes(
    node: Arc<Node<Connection>>,
    healthcheck_token: Option<String>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    let node_filter = warp::any().map(move || node.clone());
    let token_filter = warp::any().map(move || healthcheck_token.clone());

    let cors = warp::cors()
        .allow_any_origin()
        .allow_methods(vec!["GET", "OPTIONS"])
        .allow_headers(vec!["Content-Type", "Authorization", "X-Health-Token"]);

    let metrics = warp::path("metrics")
        .and(warp::get())
        .and(node_filter.clone())
        .map(handle_metrics)
        .with(cors.clone());

    let healthz = warp::path("healthz")
        .and(warp::get())
        .and(node_filter.clone())
        .map(handle_healthz)
        .with(cors.clone());

    let healthcheck = warp::path("healthcheck")
        .and(warp::get())
        .and(token_filter)
        .and(warp::header::optional::<String>("x-health-token"))
        .and(warp::header::optional::<String>("authorization"))
        .and(node_filter.clone())
        .map(handle_healthcheck)
        .with(cors);

    metrics.or(healthz).or(healthcheck)
}

fn handle_metrics(node: Arc<Node<Connection>>) -> impl Reply {
    let body = node.metrics.to_prometheus(node.node.uuid);
    warp::reply::with_header(
        body,
        "Content-Type",
        "text/plain; version=0.0.4; charset=utf-8",
    )
}

fn handle_healthz(node: Arc<Node<Connection>>) -> impl Reply {
    warp::reply::json(&serde_json::json!({
        "status": "ok",
        "node_id": node.node.uuid.to_string(),
        "hostname": node.node.hostname,
        "env": node.node.env.to_string(),
    }))
}

fn token_ok(expected: &Option<String>, header_token: Option<String>, auth: Option<String>) -> bool {
    let Some(expected) = expected.as_deref() else {
        return false; // токен не задан — ручка закрыта
    };
    if expected.is_empty() {
        return false;
    }
    let provided = header_token.or_else(|| {
        auth.and_then(|v| v.strip_prefix("Bearer ").map(|s| s.trim().to_string()))
    });
    match provided {
        Some(v) => {
            // сравнение постоянного времени по длине+байтам
            let (a, b) = (v.as_bytes(), expected.as_bytes());
            if a.len() != b.len() {
                return false;
            }
            let mut diff = 0u8;
            for (x, y) in a.iter().zip(b.iter()) {
                diff |= x ^ y;
            }
            diff == 0
        }
        None => false,
    }
}

/// Служебная информация о ноде: под токеном, без данных клиентов.
fn handle_healthcheck(
    expected: Option<String>,
    header_token: Option<String>,
    auth: Option<String>,
    node: Arc<Node<Connection>>,
) -> Box<dyn Reply> {
    if !token_ok(&expected, header_token, auth) {
        return Box::new(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({ "status": 401, "message": "unauthorized" })),
            warp::http::StatusCode::UNAUTHORIZED,
        ));
    }

    let mut system = sysinfo::System::new();
    system.refresh_cpu_usage();
    system.refresh_memory();
    let load = sysinfo::System::load_average();
    let discs = sysinfo::Disks::new_with_refreshed_list();
    let disks: Vec<serde_json::Value> = discs
        .iter()
        .map(|d| {
            let total = d.total_space();
            let avail = d.available_space();
            let used_pct = if total > 0 {
                ((total - avail) as f64 / total as f64 * 100.0).round()
            } else {
                0.0
            };
            serde_json::json!({
                "mount": d.mount_point().to_string_lossy(),
                "used_pct": used_pct,
                "total_bytes": total,
            })
        })
        .collect();

    Box::new(warp::reply::json(&serde_json::json!({
        "status": "ok",
        "node_id": node.node.uuid.to_string(),
        "hostname": node.node.hostname,
        "env": node.node.env.to_string(),
        "version": fcore::VERSION,
        "uptime_sec": fcore::metrics::process_uptime_secs().round(),
        "cpu_pct": system.global_cpu_usage().round(),
        "mem_used_bytes": system.used_memory(),
        "mem_total_bytes": system.total_memory(),
        "load": { "1": load.one, "5": load.five, "15": load.fifteen },
        "disks": disks,
        "ts": chrono::Utc::now(),
    })))
}
