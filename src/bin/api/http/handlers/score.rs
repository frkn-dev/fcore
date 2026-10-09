use std::sync::Arc;

use serde::Deserialize;
use warp::Reply;

use fcore::{
    score::{self, NodeView, ScoreConfig},
    Connection, ConnectionApiOperations, ConnectionBaseOperations, MetricStorage,
    NodeStorageOperations, SubscriptionOperations,
};

use super::admin::{check_token, not_found, unauthorized};
use crate::sync::MemSync;

#[derive(Debug, Deserialize)]
pub struct ScoreHistoryQuery {
    pub node: uuid::Uuid,
    pub from: Option<i64>,
    pub to: Option<i64>,
}

pub async fn score_list_handler<N, C, S>(
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
    metrics: Arc<MetricStorage>,
    cfg: ScoreConfig,
) -> Result<Box<dyn Reply + Send>, warp::Rejection>
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq,
{
    if !admin_enabled {
        return Ok(not_found());
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(unauthorized());
    }

    let now = chrono::Utc::now().timestamp_millis();
    let mem = memory.memory.read().await;
    let mut nodes = Vec::new();
    for (_id, node) in mem.nodes.iter_nodes() {
        let view = NodeView::from(node);
        let mut row = score::observe(&metrics, &view, &cfg, now);
        score::apply_saved_total(&metrics, &mut row);
        nodes.push(row);
    }
    nodes.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(Box::new(warp::reply::json(&serde_json::json!({
        "nodes": nodes
    }))))
}

pub async fn score_history_handler(
    query: ScoreHistoryQuery,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
    metrics: Arc<MetricStorage>,
) -> Result<Box<dyn Reply + Send>, warp::Rejection> {
    if !admin_enabled {
        return Ok(not_found());
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(unauthorized());
    }

    let now = chrono::Utc::now().timestamp_millis();
    let to = query.to.unwrap_or(now);
    let from = query.from.unwrap_or(to.saturating_sub(24 * 60 * 60 * 1000));
    let points = score::history(&metrics, &query.node, from, to);

    Ok(Box::new(warp::reply::json(&serde_json::json!({
        "node_id": query.node,
        "metric": score::SERIES_TOTAL,
        "points": points,
    }))))
}
