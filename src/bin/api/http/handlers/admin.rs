use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use warp::http::StatusCode;

use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, Env, MetricStorage, Node,
    NodeStatus, NodeStorageOperations, SubscriptionOperations,
};

use crate::sync::MemSync;

const ADMIN_HTML: &str = include_str!("../admin.html");

#[derive(Serialize)]
pub struct AdminState {
    pub nodes: NodeCounts,
    pub connections: ConnectionCounts,
    pub subscriptions: SubscriptionCounts,
}

#[derive(Serialize)]
pub struct NodeCounts {
    pub total: usize,
    pub online: usize,
    pub offline: usize,
}

#[derive(Serialize)]
pub struct ConnectionCounts {
    pub total: usize,
    pub active: usize,
}

#[derive(Serialize)]
pub struct SubscriptionCounts {
    pub total: usize,
    pub active: usize,
}

#[derive(Serialize)]
pub struct AdminNodeList {
    pub nodes: Vec<AdminNode>,
}

#[derive(Serialize, Default)]
pub struct NodeMetricsSnapshot {
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub memory_percent: Option<f64>,
    pub cpu_percent: Option<f64>,
    pub disk_usage_percent: Option<f64>,
    pub network_rx_bps: Option<f64>,
    pub network_tx_bps: Option<f64>,
}

#[derive(Serialize)]
pub struct AdminNode {
    pub id: uuid::Uuid,
    pub env: Env,
    pub hostname: String,
    pub address: String,
    pub status: NodeStatus,
    pub label: String,
    pub cluster: Option<String>,
    pub country: String,
    pub metrics: NodeMetricsSnapshot,
}

impl From<&Node> for AdminNode {
    fn from(node: &Node) -> Self {
        Self {
            id: node.uuid,
            env: node.env.clone(),
            hostname: node.hostname.clone(),
            address: node.address.to_string(),
            status: node.status,
            label: node.label.clone(),
            cluster: node.cluster.clone(),
            country: node.country.clone(),
            metrics: NodeMetricsSnapshot::default(),
        }
    }
}

#[derive(Serialize)]
pub struct AdminConnectionList {
    pub connections: Vec<AdminConnection>,
}

#[derive(Serialize)]
pub struct AdminSubscriptionList {
    pub subscriptions: Vec<AdminSubscription>,
}

#[derive(Serialize)]
pub struct AdminSubscription {
    pub id: uuid::Uuid,
    pub expires_at: Option<DateTime<Utc>>,
    pub is_active: bool,
    pub limit_bytes: Option<i64>,
    pub connections_count: usize,
    pub traffic: AdminSubscriptionTraffic,
}

#[derive(Serialize)]
pub struct AdminSubscriptionTraffic {
    pub uplink: u64,
    pub downlink: u64,
}

#[derive(Serialize)]
pub struct AdminConnection {
    pub id: uuid::Uuid,
    pub env: Env,
    pub subscription_id: Option<uuid::Uuid>,
    pub proto: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub is_deleted: bool,
    pub uplink: u64,
    pub downlink: u64,
}

fn unauthorized() -> Box<dyn warp::Reply + Send> {
    Box::new(warp::reply::with_status(
        "Unauthorized",
        StatusCode::UNAUTHORIZED,
    ))
}

fn not_found() -> Box<dyn warp::Reply + Send> {
    Box::new(warp::reply::with_status("Not found", StatusCode::NOT_FOUND))
}

fn check_token(header: Option<String>, token: &str) -> bool {
    header.is_some_and(|h| h == format!("Bearer {}", token))
}

fn check_query_token(query_token: Option<&str>, token: &str) -> bool {
    query_token == Some(token)
}

#[derive(Debug, Deserialize)]
pub struct AdminPageQuery {
    pub token: Option<String>,
}

const LOGIN_HTML: &str = r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>fcore admin</title></head>
<body style="font-family:sans-serif;max-width:400px;margin:5rem auto;text-align:center">
  <h1>fcore admin</h1>
  <p>Access denied. Open this page with the <code>?token=YOUR_ADMIN_TOKEN</code> query parameter.</p>
</body>
</html>"#;

pub async fn admin_page_handler(
    query: AdminPageQuery,
    admin_enabled: bool,
    admin_token: String,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection> {
    if !admin_enabled || admin_token.is_empty() {
        return Ok(not_found());
    }
    if !check_query_token(query.token.as_deref(), &admin_token) {
        return Ok(Box::new(warp::reply::with_header(
            LOGIN_HTML,
            "Content-Type",
            "text/html; charset=utf-8",
        )));
    }
    Ok(Box::new(warp::reply::with_header(
        ADMIN_HTML,
        "Content-Type",
        "text/html; charset=utf-8",
    )))
}

pub async fn admin_api_state_handler<N, C, S>(
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection>
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

    let mem = memory.memory.read().await;
    let now = Utc::now();

    let mut node_total = 0;
    let mut node_online = 0;
    for (_id, node) in mem.nodes.iter_nodes() {
        node_total += 1;
        if node.status == NodeStatus::Online {
            node_online += 1;
        }
    }

    let mut conn_total = 0;
    let mut conn_active = 0;
    for (_id, conn) in mem.connections.iter() {
        conn_total += 1;
        if !conn.get_deleted() {
            let active = conn.get_expires_at().map(|exp| exp > now).unwrap_or(true);
            if active {
                conn_active += 1;
            }
        }
    }

    let mut sub_total = 0;
    let mut sub_active = 0;
    for (_id, sub) in mem.subscriptions.iter() {
        sub_total += 1;
        if sub.is_active() {
            sub_active += 1;
        }
    }

    let response = AdminState {
        nodes: NodeCounts {
            total: node_total,
            online: node_online,
            offline: node_total.saturating_sub(node_online),
        },
        connections: ConnectionCounts {
            total: conn_total,
            active: conn_active,
        },
        subscriptions: SubscriptionCounts {
            total: sub_total,
            active: sub_active,
        },
    };

    Ok(Box::new(warp::reply::json(&response)))
}

pub async fn admin_api_nodes_handler<N, C, S>(
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
    metrics: Arc<MetricStorage>,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection>
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

    let mem = memory.memory.read().await;
    let mut nodes = Vec::new();

    for (_id, node) in mem.nodes.iter_nodes() {
        let mut admin_node = AdminNode::from(node);
        let (mem_used, mem_total) = metrics.node_memory(&node.uuid);
        let (net_rx, net_tx) = metrics.node_network_traffic(&node.uuid);

        admin_node.metrics = NodeMetricsSnapshot {
            memory_used_bytes: mem_used,
            memory_total_bytes: mem_total,
            memory_percent: match (mem_used, mem_total) {
                (Some(used), Some(total)) if total > 0 => {
                    Some(((used as f64 / total as f64) * 10000.0).round() / 100.0)
                }
                _ => None,
            },
            cpu_percent: metrics.node_cpu_avg(&node.uuid),
            disk_usage_percent: metrics.node_disk_usage(&node.uuid, "root"),
            network_rx_bps: net_rx,
            network_tx_bps: net_tx,
        };

        nodes.push(admin_node);
    }

    Ok(Box::new(warp::reply::json(&AdminNodeList { nodes })))
}

pub async fn admin_api_connections_handler<N, C, S>(
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection>
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

    let mem = memory.memory.read().await;
    let connections: Vec<AdminConnection> = mem
        .connections
        .iter()
        .map(|(id, conn)| AdminConnection {
            id: *id,
            env: conn.get_env(),
            subscription_id: conn.get_subscription_id(),
            proto: conn.get_proto().proto().to_string(),
            expires_at: conn.get_expires_at(),
            is_deleted: conn.get_deleted(),
            uplink: 0,
            downlink: 0,
        })
        .collect();

    Ok(Box::new(warp::reply::json(&AdminConnectionList {
        connections,
    })))
}

pub async fn admin_api_subscriptions_handler<N, C, S>(
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
    metrics: Arc<MetricStorage>,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection>
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

    let mem = memory.memory.read().await;
    let mut subscriptions = Vec::new();

    for (id, sub) in mem.subscriptions.iter() {
        let connections_count = mem
            .connections
            .iter()
            .filter(|(_, conn)| conn.get_subscription_id() == Some(*id))
            .count();
        let (uplink, downlink) = metrics.get_subscription_total_traffic(id);

        subscriptions.push(AdminSubscription {
            id: *id,
            expires_at: sub.expires_at(),
            is_active: sub.is_active(),
            limit_bytes: sub.limit_bytes(),
            connections_count,
            traffic: AdminSubscriptionTraffic { uplink, downlink },
        });
    }

    Ok(Box::new(warp::reply::json(&AdminSubscriptionList {
        subscriptions,
    })))
}

pub async fn admin_api_subscription_connections_handler<N, C, S>(
    subscription_id: uuid::Uuid,
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
    metrics: Arc<MetricStorage>,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection>
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

    let mem = memory.memory.read().await;
    let connections: Vec<(uuid::Uuid, C)> = mem
        .connections
        .iter()
        .filter(|(_, conn)| conn.get_subscription_id() == Some(subscription_id))
        .map(|(id, conn)| (*id, conn.clone()))
        .collect();

    let mut list = Vec::new();
    for (id, conn) in connections {
        let (uplink, downlink) = metrics.get_connection_total_traffic(&id);
        list.push(AdminConnection {
            id,
            env: conn.get_env(),
            subscription_id: conn.get_subscription_id(),
            proto: conn.get_proto().proto().to_string(),
            expires_at: conn.get_expires_at(),
            is_deleted: conn.get_deleted(),
            uplink,
            downlink,
        });
    }

    Ok(Box::new(warp::reply::json(&AdminConnectionList {
        connections: list,
    })))
}
