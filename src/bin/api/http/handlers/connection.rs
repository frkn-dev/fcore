use chrono::{DateTime, Utc};
use rkyv::to_bytes;
use std::net::{IpAddr, Ipv4Addr};

use tracing::{debug, error};

use fcore::{
    http::{
        helpers as http, MyRejection,
        {request::ConnType, response::Instance},
    },
    utils, Connection, ConnectionApiOperations, ConnectionBaseOperations,
    ConnectionStorageApiOperations, InboundConnLink, IpAddrMask, NodeStatus, NodeStorageOperations,
    Proto, Status, Subscription, SubscriptionOperations, SubscriptionStorageOperations, Tag, Topic,
    WgKeys, WgParam,
};

use super::super::{
    super::sync::{tasks::SyncOp, MemSync},
    param::ConnQueryParam,
    request::{ConnCreateRequest, ConnectionInfoRequest},
};

/// Handler get connection
// POST /connections/sync
pub async fn get_connections_handler<N, C, S>(
    req: ConnType,
    memory: MemSync<N, C, S>,
) -> Result<impl warp::Reply, warp::Rejection>
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
    let mem = memory.memory.read().await;
    tracing::debug!("POST /connections/sync {:?}", req.clone());

    let proto = req.proto;
    let topic = req.topic;
    let last_update = req.last_update;
    let env = req.env;

    let connections_to_send: Vec<_> = mem
        .connections
        .iter()
        .filter(|(_, conn)| {
            !conn.get_deleted()
                && conn.get_proto().proto() == proto
                && (proto == Tag::Hysteria2 || conn.get_env() == env)
                && last_update.is_none_or(|ts| conn.get_modified_at().timestamp() as u64 >= ts)
        })
        .collect();

    if connections_to_send.is_empty() {
        debug!(
            "Sending {} {:?} connections for env {:?} to topic {} ",
            connections_to_send.len(),
            proto,
            env,
            topic
        );

        return Ok(http::not_modified(""));
    }

    let messages: Vec<_> = connections_to_send
        .iter()
        .map(|(conn_id, conn)| conn.as_create_message(conn_id))
        .collect();

    if messages.is_empty() {
        return Ok(http::not_modified(""));
    }

    let bytes = to_bytes::<_, 1024>(&messages).map_err(|e| {
        error!("Serialization error: {}", e);
        warp::reject::custom(MyRejection(Box::new(e)))
    })?;

    memory
        .publisher
        .send_binary(&topic, bytes.as_ref())
        .await
        .map_err(|e| {
            error!("Publish error: {}", e);
            warp::reject::custom(MyRejection(Box::new(e)))
        })?;

    Ok(http::success_response(
        "Ok".into(),
        None,
        Instance::Count(messages.len()),
    ))
}

/// Handler creates connection
// POST /connection
pub async fn create_connection_handler<N, C, S>(
    conn_req: ConnCreateRequest,
    memory: MemSync<N, C, S>,
    wg_network: IpAddrMask,
    awg_network: IpAddrMask,
) -> Result<impl warp::Reply, warp::Rejection>
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
    if let Err(e) = conn_req.validate() {
        return Ok(http::bad_request(&e.to_string()));
    }

    let expired_at: Option<DateTime<Utc>> = conn_req
        .days
        .map(|days| Utc::now() + chrono::Duration::days(days.into()));

    let mem = memory.memory.read().await;
    if let Some(sub_id) = conn_req.subscription_id {
        let sub = mem.subscriptions.find_by_id(&sub_id);

        if sub.is_none() {
            return Ok(http::bad_request(&format!(
                "Subscription {} not found",
                sub_id
            )));
        }

        if let Some(sub) = mem.subscriptions.find_by_id(&sub_id) {
            if !sub.is_active() {
                return Ok(http::bad_request(&format!(
                    "Subscription is not active {} ",
                    sub_id
                )));
            }
        } else {
            return Ok(http::not_found(&format!(
                "Subscription {} not found",
                sub_id
            )));
        }
    }

    let proto = match conn_req.proto {
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
            let password = utils::generate_random_password(15);
            Proto::Shadowsocks { password }
        }
        Tag::VlessTcpReality
        | Tag::VlessGrpcReality
        | Tag::VlessXhttpReality
        | Tag::VlessXhttpCdn
        | Tag::Vmess => Proto::Xray(conn_req.proto),
        Tag::Hysteria2 => {
            let token = uuid::Uuid::new_v4();
            Proto::Hysteria2 { token }
        }
        Tag::Mtproto => {
            let secret = utils::generate_random_password(15);
            Proto::Mtproto { secret }
        }
    };

    drop(mem);

    let conn: Connection =
        Connection::new(&conn_req.env, conn_req.subscription_id, proto, expired_at);

    debug!("New connection to create {}", conn);
    let conn_id = uuid::Uuid::new_v4();
    let msg = conn.as_create_message(&conn_id);

    let messages = vec![msg];

    match SyncOp::add_conn(&memory, &conn_id, conn.clone()).await {
        Ok(Status::Ok(id)) => {
            let bytes = match rkyv::to_bytes::<_, 1024>(&messages) {
                Ok(b) => b,
                Err(e) => {
                    return Ok(http::internal_error(&format!("Serialization error: {}", e)));
                }
            };

            let topic = if let Some(_token) = conn.get_token() {
                // Hysteria2 uses external auth provided which handles all envs
                Some(Topic::Auth)
            } else if conn.get_proto().is_mtproto() {
                None
            } else {
                Some(conn.get_env().into())
            };

            if let Some(topic) = topic {
                let _ = memory.publisher.send_binary(&topic, bytes.as_ref()).await;
            }

            Ok(http::success_response(
                format!("Connection {} has been created", id),
                Some(id),
                Instance::Connection(conn),
            ))
        }

        Ok(Status::AlreadyExist(id)) => Ok(http::not_modified(&format!(
            "Connection {} already exists",
            id
        ))),

        Ok(Status::BadRequest(id, msg)) => {
            Ok(http::bad_request(&format!("BadRequest {} {}", id, msg)))
        }

        Ok(status) => Ok(http::bad_request(&format!(
            "Unsupported operation status: {}",
            status
        ))),

        Err(err) => Ok(http::internal_error(&format!(
            "Internal error while processing connection {}: {}",
            conn_id, err
        ))),
    }
}

/// Handler deletes connection
// DELETE /connection?id=
pub async fn delete_connection_handler<N, C, S>(
    conn_param: ConnQueryParam,

    memory: MemSync<N, C, S>,
) -> Result<impl warp::Reply, warp::Rejection>
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
    let conn_id = conn_param.id;
    let conn_opt = {
        let mem = memory.memory.read().await;
        mem.connections.get(&conn_id).cloned()
    };

    let Some(conn) = conn_opt else {
        return Ok(http::not_found(&format!(
            "Connection {} not found",
            conn_id
        )));
    };

    if conn.get_deleted() {
        return Ok(http::not_found(&format!(
            "Connection {} already is deleted",
            conn_id
        )));
    }

    match SyncOp::delete_connection(&memory, &conn_id, &conn).await {
        Ok(Status::Ok(id)) => Ok(http::success_response(
            format!("Connection {} has been deleted", id),
            Some(id),
            Instance::Connection(conn.clone().into()),
        )),

        Ok(Status::NotFound(id)) => Ok(http::not_found(&format!("Connection {} not found", id))),

        Ok(status) => Ok(http::bad_request(&format!(
            "Unsupported operation status: {}",
            status
        ))),

        Err(err) => Ok(http::internal_error(&format!(
            "Internal error while deleting connection {}: {}",
            conn_id, err
        ))),
    }
}

/// Get connection detaisl
// GET /connection?id=
pub async fn get_connection_handler<N, C, S>(
    conn_param: ConnQueryParam,
    memory: MemSync<N, C, S>,
) -> Result<impl warp::Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq
        + serde::ser::Serialize,
    S: SubscriptionOperations + Send + Sync + Clone + 'static,
    Connection: From<C>,
{
    let mem = memory.memory.read().await;

    let conn_id = conn_param.id;

    if let Some(conn) = mem.connections.get(&conn_id) {
        Ok(http::success_response(
            "Connection is found".to_string(),
            Some(conn_id),
            Instance::Connection(conn.clone().into()),
        ))
    } else {
        Ok(http::not_found("Connection is not found"))
    }
}

pub async fn wireguard_connections_handler<N, C, S>(
    req: ConnectionInfoRequest,
    memory: MemSync<N, C, S>,
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
    Connection: From<C>,
{
    if let Err(e) = req.validate() {
        return Ok(Box::new(http::bad_request(&format!("Bad Request: {}", e))));
    };

    let mem = memory.memory.read().await;

    if let Some(sub) = mem.subscriptions.find_by_id(&req.id) {
        if !sub.is_active() {
            return Ok(Box::new(http::not_found(&format!(
                "Subscription {} is expired",
                req.id
            ))));
        }
    }

    let conns = mem.connections.get_by_subscription_id(&req.id);

    if conns.is_none() {
        return Ok(Box::new(http::not_found("No connections")));
    }

    let mut result = vec![];

    if let Some(conns) = conns {
        for (conn_id, conn) in conns {
            if conn.get_deleted() || conn.get_env() != req.env {
                continue;
            }

            if conn.get_proto().proto() != Tag::Wireguard {
                continue;
            }

            if let Some(nodes) = mem.nodes.get_by_env(&conn.get_env()) {
                for node in nodes {
                    if node.status != NodeStatus::Online {
                        continue;
                    }
                    if let Some(inbound) = node.inbounds.get(&Tag::Wireguard) {
                        let c: Connection = conn.clone().into();
                        let host = node.connection_host();

                        if let Ok(link) =
                            inbound.create_link(&conn_id, &c, &node.hostname, &host, &node.label)
                        {
                            result.push(serde_json::json!({
                                "conn_id": conn_id,
                                "label": node.label,
                                "env": node.env,
                                "config": link
                            }));
                        }
                    }
                }
            }
        }
    }

    drop(mem);

    Ok(Box::new(warp::reply::json(&serde_json::json!({
        "nodes": result
    }))))
}

pub async fn amnezia_wireguard_connections_handler<N, C, S>(
    req: ConnectionInfoRequest,
    memory: MemSync<N, C, S>,
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
    Connection: From<C>,
{
    if let Err(e) = req.validate() {
        return Ok(Box::new(http::bad_request(&format!("Bad Request: {}", e))));
    };

    let mem = memory.memory.read().await;

    if let Some(sub) = mem.subscriptions.find_by_id(&req.id) {
        if !sub.is_active() {
            return Ok(Box::new(http::not_found(&format!(
                "Subscription {} is expired",
                req.id
            ))));
        }
    }

    let conns = mem.connections.get_by_subscription_id(&req.id);

    if conns.is_none() {
        return Ok(Box::new(http::not_found("No connections")));
    }

    let mut result = vec![];

    if let Some(conns) = conns {
        for (conn_id, conn) in conns {
            if conn.get_deleted() || conn.get_env() != req.env {
                continue;
            }

            if conn.get_proto().proto() != Tag::AmneziaWg {
                continue;
            }

            if let Some(nodes) = mem.nodes.get_by_env(&conn.get_env()) {
                for node in nodes {
                    if node.status != NodeStatus::Online {
                        continue;
                    }
                    if let Some(inbound) = node.inbounds.get(&Tag::AmneziaWg) {
                        let c: Connection = conn.clone().into();
                        let host = node.connection_host();

                        if let Ok(link) =
                            inbound.create_link(&conn_id, &c, &node.hostname, &host, &node.label)
                        {
                            result.push(serde_json::json!({
                                "conn_id": conn_id,
                                "label": node.label,
                                "env": node.env,
                                "config": link
                            }));
                        }
                    }
                }
            }
        }
    }

    drop(mem);

    Ok(Box::new(warp::reply::json(&serde_json::json!({
        "nodes": result
    }))))
}

pub async fn mtproto_connections_handler<N, C, S>(
    req: ConnectionInfoRequest,
    memory: MemSync<N, C, S>,
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
    Connection: From<C>,
{
    if let Err(e) = req.validate() {
        return Ok(Box::new(http::bad_request(&format!("Bad Request: {}", e))));
    };

    let mem = memory.memory.read().await;

    if let Some(sub) = mem.subscriptions.find_by_id(&req.id) {
        if !sub.is_active() {
            return Ok(Box::new(http::not_found(&format!(
                "Subscription {} is expired",
                req.id
            ))));
        }
    }

    let conns = mem.connections.get_by_subscription_id(&req.id);

    if conns.is_none() {
        return Ok(Box::new(http::not_found("No connections")));
    }

    let mut result = vec![];

    if let Some(conns) = conns {
        for (_, conn) in conns {
            if conn.get_deleted() || conn.get_env() != req.env {
                continue;
            }

            if conn.get_proto().proto() != Tag::Mtproto {
                continue;
            }

            if let Some(nodes) = mem.nodes.get_by_env(&conn.get_env()) {
                for node in nodes {
                    if node.status != NodeStatus::Online {
                        continue;
                    }
                    if let Some(inbound) = node.inbounds.get(&Tag::Mtproto) {
                        let host = node.connection_host();
                        let link = inbound.mtproto(&node.hostname, &host, &node.label);

                        if let Ok(url) = link {
                            result.push(serde_json::json!({
                                "label": node.label,
                                "url": url
                            }));
                        }
                    }
                }
            }
        }
    }

    Ok(Box::new(warp::reply::json(&serde_json::json!({
        "connections": result
    }))))
}
