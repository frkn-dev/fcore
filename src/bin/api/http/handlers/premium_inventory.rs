//! Premium inventory for the admin cabinet.
//!
//! There is no separate slot registry: a premium node announces its env on
//! startup (`custompremium-…`), so the inventory is exactly "premium envs we
//! currently have nodes for" plus "which subscription holds each of them".
//!
//! Two states matter to support:
//!   * assigned — a live subscription has this env as its `scope_env`; the row
//!     carries `subscription_id` and `premium_until`, so support can ask the
//!     person whether they are renewing;
//!   * free — the env has nodes but no subscription points at it, so the slot
//!     can be handed out.
//!
//! `expired` marks an assigned slot whose premium term is over: its
//! connections are cut by `cleanup_expired_subscriptions`, but the row keeps
//! `subscription_id` on purpose — the slot is empty, yet we still know who had
//! it.

use chrono::{DateTime, Utc};
use warp::Reply;

use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, ConnectionStorageApiOperations,
    NodeStatus, NodeStorageOperations, NodeType, SubscriptionOperations,
};

use crate::http::handlers::admin::{check_token, not_found, unauthorized};
use crate::sync::MemSync;

pub async fn admin_premium_inventory_handler<N, C, S>(
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
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

    let now = Utc::now();
    let mem = memory.memory.read().await;

    // env -> nodes (premium envs only; the node itself is the source of truth)
    let mut env_nodes: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
        std::collections::BTreeMap::new();
    for (_id, node) in mem.nodes.iter_nodes() {
        // Слот — это premium-энв ИЛИ нода на премиум-хосте. На prem1 пять
        // слотов живут personal-энвами (каждый клиент получил свой), а
        // node_type при регистрации приходит «common» — fnode его не шлёт,
        // поэтому единственный надёжный признак для них — имя хоста prem*.
        let on_premium_host =
            node.hostname.starts_with("prem") || matches!(node.r#type, NodeType::PremiumNode);
        if !node.env.is_premium() && !on_premium_host {
            continue;
        }
        let res = node.as_node_response();
        env_nodes
            .entry(node.env.to_string())
            .or_default()
            .push(serde_json::json!({
                "uuid": res.uuid,
                "hostname": res.hostname,
                "address": res.address.to_string(),
                "label": res.label,
                "country": res.country,
                "status": match res.status {
                    NodeStatus::Online => "online",
                    NodeStatus::Offline => "offline",
                },
            }));
    }

    // env -> (owner subscription, premium term)
    let mut owners: std::collections::BTreeMap<String, (uuid::Uuid, Option<DateTime<Utc>>)> =
        std::collections::BTreeMap::new();
    for (id, sub) in mem.subscriptions.iter() {
        if sub.is_deleted() {
            continue;
        }
        // Владельцем может быть и personal-энв: на prem1 слоты выданы именно
        // так, поэтому привязку ищем по любому scope_env, а не только premium.
        if let Some(env) = sub.scope_env() {
            owners.insert(env.to_string(), (*id, sub.premium_until()));
        }
    }

    let mut slots = Vec::new();
    let mut expired_count = 0usize;

    for (env, nodes) in &env_nodes {
        let owner = owners.get(env);
        let premium_until = owner.and_then(|(_, until)| *until);
        let expired = premium_until.map(|until| until <= now).unwrap_or(false);
        if expired {
            expired_count += 1;
        }

        // Live connections on this env — "is the slot actually in use".
        let mut conns_total = 0usize;
        let mut conns_active = 0usize;
        if let Some((sub_id, _)) = owner {
            if let Some(conns) = mem.connections.get_by_subscription_id(sub_id) {
                for (_cid, conn) in conns.iter() {
                    if conn.get_deleted() || conn.get_env().to_string() != *env {
                        continue;
                    }
                    conns_total += 1;
                    if conn.get_expires_at().map(|e| e > now).unwrap_or(true) {
                        conns_active += 1;
                    }
                }
            }
        }

        // Трафик слота = трафик подписки-владельца: отдельного счётчика на
        // слот нет, но клиент платит именно за этот канал.
        let (traffic_up, traffic_down) = match owner {
            Some((sub_id, _)) => memory
                .db
                .traffic()
                .total_for_subscription(*sub_id)
                .await
                .unwrap_or((0, 0)),
            None => (0, 0),
        };

        slots.push(serde_json::json!({
            "env": env,
            "subscription_id": owner.map(|(id, _)| *id),
            "premium_until": premium_until,
            "expired": expired,
            "connections_total": conns_total,
            "connections_active": conns_active,
            "traffic_up": traffic_up,
            "traffic_down": traffic_down,
            "traffic_total": traffic_up + traffic_down,
            "nodes": nodes,
        }));
    }

    let assigned = slots
        .iter()
        .filter(|s| s["subscription_id"].is_string())
        .count();
    let free = slots
        .iter()
        .filter(|s| s["subscription_id"].is_null())
        .count();

    Ok(Box::new(warp::reply::json(&serde_json::json!({
        "status": 200,
        "totals": {
            "slots": slots.len(),
            "assigned": assigned,
            "free": free,
            "expired": expired_count,
        },
        "slots": slots,
    }))))
}
