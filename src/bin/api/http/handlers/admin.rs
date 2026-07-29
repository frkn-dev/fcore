use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::error;
use warp::http::StatusCode;

use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, Env, MetricStorage, Node,
    NodeStatus, NodeStorageOperations, Status, Subscription, SubscriptionOperations, SubscriptionStorageOperations,
};

use crate::sync::{tasks::SyncOp, MemSync};

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
    /// Composite load percentage derived from the maximum of CPU, memory and
    /// disk usage. A higher value indicates the node is closer to saturation.
    pub load_percent: Option<f64>,
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
    pub refer_code: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub is_active: bool,
    pub limit_bytes: Option<i64>,
    pub connections_count: usize,
    pub online: u64,
    pub traffic: AdminSubscriptionTraffic,
}

#[derive(Debug, Serialize)]
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
    pub online: u64,
}

pub fn unauthorized() -> Box<dyn warp::Reply + Send> {
    Box::new(warp::reply::with_status(
        "Unauthorized",
        StatusCode::UNAUTHORIZED,
    ))
}

pub fn not_found() -> Box<dyn warp::Reply + Send> {
    Box::new(warp::reply::with_status("Not found", StatusCode::NOT_FOUND))
}

pub fn check_token(header: Option<String>, token: &str) -> bool {
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

        let cpu_percent = metrics.node_cpu_avg(&node.uuid);
        let memory_percent = match (mem_used, mem_total) {
            (Some(used), Some(total)) if total > 0 => {
                Some(((used as f64 / total as f64) * 10000.0).round() / 100.0)
            }
            _ => None,
        };
        let disk_usage_percent = metrics.node_disk_usage(&node.uuid, "root");

        let load_percent = [
            cpu_percent,
            memory_percent,
            disk_usage_percent,
        ]
        .iter()
        .filter_map(|v| *v)
        .reduce(f64::max);

        admin_node.metrics = NodeMetricsSnapshot {
            memory_used_bytes: mem_used,
            memory_total_bytes: mem_total,
            memory_percent,
            cpu_percent,
            disk_usage_percent,
            network_rx_bps: net_rx,
            network_tx_bps: net_tx,
            load_percent,
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
    let mut connections: Vec<AdminConnection> = mem
        .connections
        .iter()
        .map(|(id, conn)| {
            let online = metrics.get_connection_online_count(id);
            AdminConnection {
                id: *id,
                env: conn.get_env(),
                subscription_id: conn.get_subscription_id(),
                proto: conn.get_proto().proto().to_string(),
                expires_at: conn.get_expires_at(),
                is_deleted: conn.get_deleted(),
                uplink: 0,
                downlink: 0,
                online,
            }
        })
        .collect();

    for conn in &mut connections {
        let (uplink, downlink) = metrics.get_connection_total_traffic(&conn.id);
        conn.uplink = uplink;
        conn.downlink = downlink;
    }

    Ok(Box::new(warp::reply::json(&AdminConnectionList {
        connections,
    })))
}

fn count_subscriptions<'a, S>(subs: impl Iterator<Item = &'a S>) -> SubscriptionCounts
where
    S: SubscriptionOperations + 'a,
{
    let mut total = 0;
    let mut active = 0;
    for sub in subs {
        if sub.is_deleted() {
            continue;
        }
        total += 1;
        if sub.is_active() {
            active += 1;
        }
    }
    SubscriptionCounts { total, active }
}

pub async fn admin_api_subscriptions_count_handler<N, C, S>(
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
    let counts = count_subscriptions(mem.subscriptions.iter().map(|(_, sub)| sub));

    Ok(Box::new(warp::reply::json(&counts)))
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
        let online = metrics.get_subscription_online_count(id);

        subscriptions.push(AdminSubscription {
            id: *id,
            refer_code: sub.refer_code(),
            expires_at: sub.expires_at(),
            is_active: sub.is_active(),
            limit_bytes: sub.limit_bytes(),
            connections_count,
            online,
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
        let online = metrics.get_connection_online_count(&id);
        list.push(AdminConnection {
            id,
            env: conn.get_env(),
            subscription_id: conn.get_subscription_id(),
            proto: conn.get_proto().proto().to_string(),
            expires_at: conn.get_expires_at(),
            is_deleted: conn.get_deleted(),
            uplink,
            downlink,
            online,
        });
    }

    Ok(Box::new(warp::reply::json(&AdminConnectionList {
        connections: list,
    })))
}

#[derive(Debug, Deserialize)]
pub struct AdminAssignPremiumRequest {
    pub env: Env,
}

#[derive(Debug, Deserialize)]
pub struct AdminNodeMetricsQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub metric: Option<String>,
}

#[derive(Serialize)]
pub struct AdminNodeMetricsResponse {
    pub metrics: Vec<AdminNodeMetricSeries>,
}

#[derive(Serialize)]
pub struct AdminNodeMetricSeries {
    pub name: String,
    pub tags: BTreeMap<String, String>,
    pub points: Vec<fcore::MetricPoint>,
}

pub async fn admin_api_node_metrics_handler(
    node_id: uuid::Uuid,
    query: AdminNodeMetricsQuery,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
    metrics: Arc<MetricStorage>,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection> {
    if !admin_enabled {
        return Ok(not_found());
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(unauthorized());
    }

    let now = chrono::Utc::now().timestamp_millis();
    let from = query.from.unwrap_or(now - 600_000);
    let to = query.to.unwrap_or(now);

    let Some(node_map) = metrics.inner.get(&node_id) else {
        return Ok(Box::new(warp::reply::json(&AdminNodeMetricsResponse { metrics: vec![] },
        )));
    };

    let mut result = Vec::new();

    for entry in node_map.iter() {
        let hash = *entry.key();
        let Some(meta) = metrics.metadata.get(&hash) else {
            continue;
        };
        let (name, tags) = meta.value().clone();

        if matches!(
            name.as_str(),
            "user.traffic.downlink" | "user.traffic.uplink" | "user.traffic.online"
        ) {
            continue;
        }

        if let Some(ref filter) = query.metric {
            if !name.starts_with(filter) {
                continue;
            }
        }

        let points: Vec<fcore::MetricPoint> = entry
            .value()
            .iter()
            .filter(|p| p.timestamp >= from && p.timestamp <= to)
            .cloned()
            .collect();

        if points.is_empty() {
            continue;
        }

        result.push(AdminNodeMetricSeries {
            name,
            tags,
            points,
        });
    }

    Ok(Box::new(warp::reply::json(&AdminNodeMetricsResponse { metrics: result },
    )))
}

pub async fn admin_api_assign_premium_handler<N, C, S>(
    subscription_id: uuid::Uuid,
    req: AdminAssignPremiumRequest,
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
        return Ok(Box::new(warp::reply::with_status(
            "Admin disabled",
            StatusCode::FORBIDDEN,
        )));
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(Box::new(warp::reply::with_status(
            "Unauthorized",
            StatusCode::UNAUTHORIZED,
        )));
    }

    let premium_token = format!("prem_{}", uuid::Uuid::new_v4().simple());

    let env = req.env.clone();
    {
        let mut mem = memory.memory.write().await;
        let sub = match mem.subscriptions.find_by_id_mut(&subscription_id) {
            Some(s) => s,
            None => return Ok(Box::new(warp::reply::with_status(
                "Subscription not found",
                StatusCode::NOT_FOUND,
            ))),
        };
        sub.set_scope_env(env.clone());
        sub.set_premium_token(premium_token.clone());
    }

    if let Err(e) = memory
        .db
        .sub()
        .set_premium_fields(&subscription_id, Some(&env), Some(&premium_token))
        .await
    {
        error!("Failed to set premium fields: {}", e);
        return Ok(Box::new(warp::reply::with_status(
            "Database error",
            StatusCode::INTERNAL_SERVER_ERROR,
        )));
    }

    Ok(Box::new(warp::reply::json(&serde_json::json!({
        "premium_token": premium_token
    }))))
}

#[derive(Debug, Deserialize)]
pub struct AdminCreateSubscriptionRequest {
    pub days: i64,
    pub limit_bytes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AdminExtendSubscriptionRequest {
    pub days: i64,
}

pub async fn admin_api_create_subscription_handler<N, C, S>(
    req: AdminCreateSubscriptionRequest,
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
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if !admin_enabled {
        return Ok(not_found());
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(unauthorized());
    }

    if req.days <= 0 {
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"status": 400, "message": "days must be greater than 0"}),
            ),
            StatusCode::BAD_REQUEST,
        )));
    }

    let sub_id = uuid::Uuid::new_v4();
    let ref_code = fcore::utils::get_uuid_last_octet_simple(&sub_id);
    let expires_at = Utc::now() + chrono::Duration::days(req.days);
    let sub = Subscription::new(sub_id, ref_code, Some(expires_at), req.limit_bytes);

    match SyncOp::add_sub(&memory, sub.clone())
        .await
    {
        Ok(Status::Ok(_)) | Ok(Status::Updated(_)) => Ok(Box::new(warp::reply::json(
            &serde_json::json!({"id": sub_id, "ref_code": sub.refer_code()}),
        ))),
        Ok(Status::AlreadyExist(_)) => Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"status": 409, "message": "Subscription already exists"}),
            ),
            StatusCode::CONFLICT,
        ))),
        Ok(_) => Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"status": 400, "message": "Failed to create subscription"}),
            ),
            StatusCode::BAD_REQUEST,
        ))),
        Err(e) => {
            error!("Failed to create subscription: {:?}", e);
            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(
                    &serde_json::json!({"status": 500, "message": "Internal error"}),
                ),
                StatusCode::INTERNAL_SERVER_ERROR,
            )))
        }
    }
}

pub async fn admin_api_extend_subscription_handler<N, C, S>(
    subscription_id: uuid::Uuid,
    req: AdminExtendSubscriptionRequest,
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
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if !admin_enabled {
        return Ok(not_found());
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(unauthorized());
    }

    if req.days <= 0 {
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"status": 400, "message": "days must be greater than 0"}),
            ),
            StatusCode::BAD_REQUEST,
        )));
    }

    match SyncOp::add_days(&memory, &subscription_id, req.days)
        .await
    {
        Ok(Status::Updated(_)) => Ok(Box::new(warp::reply::json(
            &serde_json::json!({"id": subscription_id, "days_added": req.days}),
        ))),
        Ok(Status::NotFound(_)) => Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"status": 404, "message": "Subscription not found"}),
            ),
            StatusCode::NOT_FOUND,
        ))),
        Ok(_) => Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"status": 400, "message": "Failed to extend subscription"}),
            ),
            StatusCode::BAD_REQUEST,
        ))),
        Err(e) => {
            error!("Failed to extend subscription: {:?}", e);
            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(
                    &serde_json::json!({"status": 500, "message": "Internal error"}),
                ),
                StatusCode::INTERNAL_SERVER_ERROR,
            )))
        }
    }
}

pub async fn admin_api_delete_subscription_handler<N, C, S>(
    subscription_id: uuid::Uuid,
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
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if !admin_enabled {
        return Ok(not_found());
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(unauthorized());
    }

    let conn_ids: Vec<uuid::Uuid> = {
        let mem = memory.memory.read().await;
        mem.connections
            .iter()
            .filter(|(_, conn)| conn.get_subscription_id() == Some(subscription_id) && !conn.get_deleted())
            .map(|(id, _)| *id)
            .collect()
    };

    for conn_id in conn_ids {
        let conn_opt = {
            let mem = memory.memory.read().await;
            mem.connections.get(&conn_id).cloned()
        };
        if let Some(conn) = conn_opt {
            if let Err(e) = SyncOp::delete_connection(&memory, &conn_id, &conn).await {
                error!("Failed to delete connection {} for subscription {}: {:?}", conn_id, subscription_id, e);
            }
        }
    }

    {
        let mut mem = memory.memory.write().await;
        if let Some(sub) = mem.subscriptions.find_by_id_mut(&subscription_id) {
            sub.mark_deleted();
        } else {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(
                    &serde_json::json!({"status": 404, "message": "Subscription not found"}),
                ),
                StatusCode::NOT_FOUND,
            )));
        }
    }

    if let Err(e) = memory.db.sub().delete(&subscription_id).await {
        error!("Failed to delete subscription {} from database: {:?}", subscription_id, e);
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"status": 500, "message": "Database error"}),
            ),
            StatusCode::INTERNAL_SERVER_ERROR,
        )));
    }

    Ok(Box::new(warp::reply::json(
        &serde_json::json!({"id": subscription_id, "deleted": true}),
    )))
}

pub async fn admin_api_delete_connection_handler<N, C, S>(
    connection_id: uuid::Uuid,
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
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if !admin_enabled {
        return Ok(not_found());
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(unauthorized());
    }

    let conn_opt = {
        let mem = memory.memory.read().await;
        mem.connections.get(&connection_id).cloned()
    };

    let Some(conn) = conn_opt else {
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"status": 404, "message": "Connection not found"}),
            ),
            StatusCode::NOT_FOUND,
        )));
    };

    if conn.get_deleted() {
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"status": 400, "message": "Connection already deleted"}),
            ),
            StatusCode::BAD_REQUEST,
        )));
    }

    match SyncOp::delete_connection(&memory, &connection_id, &conn).await {
        Ok(Status::Ok(_)) => Ok(Box::new(warp::reply::json(
            &serde_json::json!({"id": connection_id, "deleted": true}),
        ))),
        Ok(Status::NotFound(_)) => Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"status": 404, "message": "Connection not found"}),
            ),
            StatusCode::NOT_FOUND,
        ))),
        Ok(_) => Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"status": 400, "message": "Failed to delete connection"}),
            ),
            StatusCode::BAD_REQUEST,
        ))),
        Err(e) => {
            error!("Failed to delete connection {}: {:?}", connection_id, e);
            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(
                    &serde_json::json!({"status": 500, "message": "Internal error"}),
                ),
                StatusCode::INTERNAL_SERVER_ERROR,
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn sub(expires_at: Option<DateTime<Utc>>, deleted: bool) -> Subscription {
        let id = uuid::Uuid::new_v4();
        let mut s = Subscription::new(id, "ref".to_string(), expires_at, None);
        if deleted {
            s.mark_deleted();
        }
        s
    }

    #[test]
    fn count_subscriptions_skips_deleted_and_counts_active() {
        let now = Utc::now();
        let subs = vec![
            sub(Some(now + Duration::days(1)), false),  // active
            sub(Some(now - Duration::days(1)), false),  // expired
            sub(Some(now + Duration::days(1)), true),   // deleted
            sub(None, false),                           // no expiry => active
        ];

        let counts = count_subscriptions(subs.iter());

        assert_eq!(counts.total, 3);
        assert_eq!(counts.active, 2);
    }
}
