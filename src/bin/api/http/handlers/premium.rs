use chrono::{DateTime, Utc};
use tracing::Instrument;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use uuid::Uuid;
use warp::{http::StatusCode, Rejection, Reply};

use super::super::super::{
    subscription_audit,
    sync::{tasks::SyncOp, MemSync},
};
use super::admin::AdminSubscriptionTraffic;
use super::subscription::build_subscription_traffic;

use fcore::{
    http::helpers as http, Connection, ConnectionApiOperations, ConnectionBaseOperations,
    ConnectionStorageApiOperations, ConnectionStorageBaseOperations, Env, IpAddrMask,
    MetricStorage, NodeStorageOperations, Proto, Status, Subscription, SubscriptionOperations,
    SubscriptionStorageOperations, Tag, Topic, WgKeys, WgParam,
};

#[derive(Debug, Deserialize)]
pub struct PremiumChildCreateRequest {
    pub days: Option<i64>,
    pub limit_bytes: Option<i64>,
}

impl PremiumChildCreateRequest {
    pub fn validate(&self) -> Result<(), String> {
        match self.days {
            Some(days) if days > 0 => Ok(()),
            Some(_) => Err("days must be greater than 0".to_string()),
            None => Err("days is required".to_string()),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PremiumChildUpdateRequest {
    pub days: Option<i64>,
    pub limit_bytes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct PremiumConnectionCreateRequest {
    pub env: Env,
    pub proto: Tag,
    pub days: Option<u16>,
}

#[derive(Debug, Serialize)]
pub struct PremiumChild {
    pub id: Uuid,
    pub refer_code: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub limit_bytes: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PremiumConnection {
    pub id: Uuid,
    pub env: String,
    pub proto: String,
    pub subscription_id: Uuid,
    pub is_deleted: bool,
    pub traffic: AdminSubscriptionTraffic,
}

#[derive(Debug, Serialize)]
pub struct PremiumState {
    pub children_count: usize,
    pub active_children: usize,
    pub connections_count: usize,
    pub total_traffic: AdminSubscriptionTraffic,
}

fn ensure_scope<S: SubscriptionOperations>(parent: &S, env: &Env) -> Result<(), Rejection> {
    if let Some(scope) = parent.scope_env() {
        if scope != env {
            return Err(warp::reject::custom(fcore::http::AuthError(
                "Env out of scope".to_string(),
            )));
        }
    }
    Ok(())
}

fn ensure_child_of<S: SubscriptionOperations>(parent: &S, child: &S) -> Result<(), Rejection> {
    if child.parent_id() != Some(parent.id()) {
        return Err(warp::reject::custom(fcore::http::AuthError(
            "Forbidden".to_string(),
        )));
    }
    Ok(())
}

pub async fn premium_state_handler<N, C, S>(
    parent: S,
    memory: MemSync<N, C, S>,
    metrics: Arc<MetricStorage>,
) -> Result<impl Reply, Rejection>
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let mem = memory.memory.read().await;
    let children = mem.subscriptions.find_by_parent_id(&parent.id());

    let mut total_uplink = 0u64;
    let mut total_downlink = 0u64;
    let mut connections_count = 0usize;

    for child in &children {
        let conns = mem.connections.get_by_subscription_id(&child.id());
        if let Some(conns) = conns {
            for (conn_id, _) in conns {
                let (up, down) = metrics.get_connection_total_traffic(&conn_id);
                total_uplink += up;
                total_downlink += down;
                connections_count += 1;
            }
        }
    }

    let active_children = children.iter().filter(|c| c.is_active()).count();

    Ok(warp::reply::json(&PremiumState {
        children_count: children.len(),
        active_children,
        connections_count,
        total_traffic: AdminSubscriptionTraffic {
            uplink: total_uplink,
            downlink: total_downlink,
        },
    }))
}

pub async fn premium_child_list_handler<N, C, S>(
    parent: S,
    memory: MemSync<N, C, S>,
) -> Result<impl Reply, Rejection>
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let mem = memory.memory.read().await;
    let children: Vec<PremiumChild> = mem
        .subscriptions
        .find_by_parent_id(&parent.id())
        .into_iter()
        .map(|s| PremiumChild {
            id: s.id(),
            refer_code: s.refer_code(),
            expires_at: s.expires_at(),
            is_active: s.is_active(),
            created_at: s.created_at(),
            limit_bytes: s.limit_bytes(),
        })
        .collect();

    Ok(warp::reply::json(&children))
}

pub async fn premium_create_child_handler<N, C, S>(
    parent: S,
    req: PremiumChildCreateRequest,
    trace_id_header: Option<String>,
    memory: MemSync<N, C, S>,
) -> Result<impl Reply, Rejection>
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
    let trace_id = subscription_audit::trace_id_from_header(trace_id_header);
    let sub_id = Uuid::new_v4();
    let ref_code = fcore::utils::get_uuid_last_octet_simple(&sub_id);

    if let Err(e) = req.validate() {
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"status": 400, "message": e}),
            ),
            warp::http::StatusCode::BAD_REQUEST,
        )));
    }

    let expires_at = req.days.map(|d| Utc::now() + chrono::Duration::days(d));

    let mut sub = Subscription::new(sub_id, ref_code, expires_at, req.limit_bytes);
    sub.set_parent_id(parent.id());

    subscription_audit::log_transaction_start(sub_id, req.days);

    match SyncOp::add_sub(&memory, sub)
        .instrument(subscription_audit::transaction_span(
            "premium_create_child_handler",
            sub_id,
            Some(trace_id),
        ))
        .await
    {
        Ok(_) => Ok(Box::new(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({ "id": sub_id })),
            StatusCode::CREATED,
        ))),
        Err(e) => {
            tracing::error!("Failed to create premium child subscription: {:?}", e);
            Ok(Box::new(http::internal_error(&format!("{}", e))))
        }
    }
}

pub async fn premium_update_child_handler<N, C, S>(
    parent: S,
    child_id: Uuid,
    req: PremiumChildUpdateRequest,
    trace_id_header: Option<String>,
    memory: MemSync<N, C, S>,
) -> Result<impl Reply, Rejection>
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
    let trace_id = subscription_audit::trace_id_from_header(trace_id_header);
    {
        let mem = memory.memory.read().await;
        let child = mem.subscriptions.find_by_id(&child_id).ok_or_else(|| {
            warp::reject::custom(fcore::Error::Custom("Child not found".to_string()))
        })?;
        ensure_child_of(&parent, child)?;
    }

    let update_req = super::super::request::Subscription {
        days: req.days,
        refer_code: None,
        limit_bytes: req.limit_bytes,
    };

    subscription_audit::log_transaction_start(child_id, req.days);

    match SyncOp::update_sub(
        &memory,
        &child_id,
        update_req,
    )
    .instrument(subscription_audit::transaction_span(
        "premium_update_child_handler",
        child_id,
        Some(trace_id),
    ))
    .await
    {
        Ok(_) => Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({ "id": child_id })),
            StatusCode::OK,
        )),
        Err(e) => {
            tracing::error!("Failed to update premium child subscription: {:?}", e);
            Ok(http::internal_error(&format!("{}", e)))
        }
    }
}

pub async fn premium_child_connections_handler<N, C, S>(
    parent: S,
    child_id: Uuid,
    memory: MemSync<N, C, S>,
    metrics: Arc<MetricStorage>,
) -> Result<impl Reply, Rejection>
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let mem = memory.memory.read().await;
    let child = mem.subscriptions.find_by_id(&child_id).ok_or_else(|| {
        warp::reject::custom(fcore::Error::Custom("Child not found".to_string()))
    })?;
    ensure_child_of(&parent, child)?;

    let conns = mem.connections.get_by_subscription_id(&child_id);

    let result: Vec<PremiumConnection> = match conns {
        Some(cs) => cs
            .iter()
            .map(|(id, c)| {
                let (uplink, downlink) = metrics.get_connection_total_traffic(id);
                PremiumConnection {
                    id: *id,
                    env: c.get_env().to_string(),
                    proto: c.get_proto().proto().to_string(),
                    subscription_id: child_id,
                    is_deleted: c.get_deleted(),
                    traffic: AdminSubscriptionTraffic { uplink, downlink },
                }
            })
            .collect(),
        None => Vec::new(),
    };

    Ok(warp::reply::json(&result))
}

pub async fn premium_create_connection_handler<N, C, S>(
    parent: S,
    child_id: Uuid,
    req: PremiumConnectionCreateRequest,
    memory: MemSync<N, C, S>,
    wg_network: IpAddrMask,
    awg_network: IpAddrMask,
) -> Result<impl Reply, Rejection>
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
    ensure_scope(&parent, &req.env)?;

    let proto = {
        let mem = memory.memory.read().await;
        let child = mem.subscriptions.find_by_id(&child_id).ok_or_else(|| {
            warp::reject::custom(fcore::Error::Custom("Child not found".to_string()))
        })?;
        ensure_child_of(&parent, child)?;
        if !child.is_active() {
            return Err(warp::reject::custom(fcore::Error::Custom(
                "Child subscription is not active".to_string(),
            )));
        }

        match req.proto {
            Tag::Wireguard => {
                let last_ip: Option<Ipv4Addr> = mem
                    .connections
                    .get_last_wg_addr()
                    .and_then(|mask| mask.as_ipv4());

                let next = match last_ip {
                    Some(ip) => IpAddrMask::increment_ipv4(ip),
                    None => wg_network.first_peer_ip(),
                };

                let next = match next {
                    Some(ip) => ip,
                    None => return Ok(http::internal_error("Failed to allocate IP")),
                };

                if !wg_network.contains_ipv4(next) {
                    return Ok(http::internal_error("IP out of range"));
                }

                Proto::Wireguard {
                    param: WgParam {
                        keys: WgKeys::default(),
                        address: IpAddrMask {
                            address: IpAddr::V4(next),
                            cidr: 32,
                        },
                    },
                }
            }
            Tag::AmneziaWg => {
                let last_ip: Option<Ipv4Addr> = mem
                    .connections
                    .get_last_awg_addr()
                    .and_then(|mask| mask.as_ipv4());

                let next = match last_ip {
                    Some(ip) => IpAddrMask::increment_ipv4(ip),
                    None => awg_network.first_peer_ip(),
                };

                let next = match next {
                    Some(ip) => ip,
                    None => return Ok(http::internal_error("Failed to allocate IP")),
                };

                if !awg_network.contains_ipv4(next) {
                    return Ok(http::internal_error("IP out of range"));
                }

                Proto::AmneziaWg {
                    param: WgParam {
                        keys: WgKeys::default(),
                        address: IpAddrMask {
                            address: IpAddr::V4(next),
                            cidr: 32,
                        },
                    },
                }
            }
            Tag::Shadowsocks => {
                let password = fcore::utils::generate_random_password(15);
                Proto::Shadowsocks { password }
            }
            Tag::VlessTcpReality
            | Tag::VlessGrpcReality
            | Tag::VlessXhttpReality
            | Tag::VlessXhttpCdn
            | Tag::Vmess => Proto::Xray(req.proto),
            Tag::Hysteria2 => {
                let token = Uuid::new_v4();
                Proto::Hysteria2 { token }
            }
            Tag::Mtproto => {
                let secret = fcore::utils::generate_random_password(15);
                Proto::Mtproto { secret }
            }
        }
    };

    let conn_id = Uuid::new_v4();
    let expires_at = req.days.map(|d| Utc::now() + chrono::Duration::days(d as i64));
    let conn = Connection::new(&req.env, Some(child_id), proto, expires_at);

    match SyncOp::add_conn(&memory, &conn_id, conn.clone()).await {
        Ok(Status::Ok(id)) | Ok(Status::AlreadyExist(id)) => {
            let msg = vec![conn.as_create_message(&conn_id)];
            let topic = if conn.get_token().is_some() {
                Some(Topic::Auth)
            } else if conn.get_proto().is_mtproto() {
                None
            } else {
                Some(conn.get_env().into())
            };

            if let Some(topic) = topic {
                let bytes = match rkyv::to_bytes::<_, 1024>(&msg) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::error!("Serialization error: {}", e);
                        return Ok(http::internal_error(&format!("Serialization error: {}", e)));
                    }
                };
                let _ = memory.publisher.send_binary(&topic, bytes.as_ref()).await;
            }

            Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({ "id": id })),
                StatusCode::CREATED,
            ))
        }
        Ok(Status::BadRequest(id, msg)) => Ok(http::bad_request(&format!(
            "BadRequest {} {}",
            id, msg
        ))),
        Ok(status) => Ok(http::bad_request(&format!(
            "Unsupported operation status: {}",
            status
        ))),
        Err(err) => {
            tracing::error!("Internal error while processing connection {}: {}", conn_id, err);
            Ok(http::internal_error(&format!(
                "Internal error while processing connection {}: {}",
                conn_id, err
            )))
        }
    }
}

pub async fn premium_delete_connection_handler<N, C, S>(
    parent: S,
    conn_id: Uuid,
    memory: MemSync<N, C, S>,
) -> Result<impl Reply, Rejection>
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
    let conn: C = {
        let mem = memory.memory.read().await;
        let conn = mem.connections.get(&conn_id).ok_or_else(|| {
            warp::reject::custom(fcore::Error::Custom("Connection not found".to_string()))
        })?;
        let sub_id = conn
            .get_subscription_id()
            .ok_or_else(|| warp::reject::custom(fcore::Error::Custom("Invalid connection".to_string())))?;
        let child = mem.subscriptions.find_by_id(&sub_id).ok_or_else(|| {
            warp::reject::custom(fcore::Error::Custom("Subscription not found".to_string()))
        })?;
        ensure_child_of(&parent, child)?;
        conn.clone()
    };

    match SyncOp::delete_connection(&memory, &conn_id, &conn).await {
        Ok(_) => Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({})),
            StatusCode::NO_CONTENT,
        )),
        Err(e) => {
            tracing::error!("Failed to delete premium connection: {:?}", e);
            Ok(http::internal_error(&format!("{}", e)))
        }
    }
}

pub async fn premium_child_traffic_handler<N, C, S>(
    parent: S,
    child_id: Uuid,
    memory: MemSync<N, C, S>,
    metrics: Arc<MetricStorage>,
) -> Result<impl Reply, Rejection>
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let created_at = {
        let mem = memory.memory.read().await;
        let child = mem.subscriptions.find_by_id(&child_id).ok_or_else(|| {
            warp::reject::custom(fcore::Error::Custom("Child not found".to_string()))
        })?;
        ensure_child_of(&parent, child)?;
        child.created_at()
    };

    let traffic = build_subscription_traffic(&memory.db, &metrics, child_id, created_at).await?;
    Ok(warp::reply::json(&traffic))
}
