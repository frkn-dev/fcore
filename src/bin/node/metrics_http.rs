use std::sync::Arc;

use warp::{Filter, Rejection, Reply};

use crate::node::Node;
use fcore::BaseConnection as Connection;

pub fn routes(
    node: Arc<Node<Connection>>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    let node_filter = warp::any().map(move || node.clone());

    let cors = warp::cors()
        .allow_any_origin()
        .allow_methods(vec!["GET", "OPTIONS"])
        .allow_headers(vec!["Content-Type", "Authorization"]);

    let metrics = warp::path("metrics")
        .and(warp::get())
        .and(node_filter.clone())
        .map(handle_metrics)
        .with(cors.clone());

    let healthz = warp::path("healthz")
        .and(warp::get())
        .and(node_filter.clone())
        .map(handle_healthz)
        .with(cors);

    metrics.or(healthz)
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
