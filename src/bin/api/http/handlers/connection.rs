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
        .filter(|(conn_id, conn)| {
            !conn.get_deleted()
                && conn.get_proto().proto() == proto
                && (proto == Tag::Hysteria2 || conn.get_env() == env)
                && last_update.is_none_or(|ts| conn.get_modified_at().timestamp() as u64 >= ts)
                // A node-pinned conn is catch-up visible only to its node's
                // Init topic; on every other topic pinned conns are hidden.
                && sync_conn_visible(mem.conn_nodes.get(conn_id), &topic)
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

/// Internal helper to create a connection. Returns the connection id or an error message.
pub async fn create_connection_inner<N, C, S>(
    env: &fcore::Env,
    proto: fcore::Tag,
    subscription_id: Option<uuid::Uuid>,
    days: Option<u16>,
    label: Option<String>,
    node_id: Option<uuid::Uuid>,
    memory: &MemSync<N, C, S>,
    wg_network: &IpAddrMask,
    awg_network: &IpAddrMask,
    awg_mobile_network: &Option<IpAddrMask>,
) -> Result<(uuid::Uuid, Connection), String>
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
    let expired_at: Option<DateTime<Utc>> = days
        .map(|d| Utc::now() + chrono::Duration::days(d.into()));

    let mem = memory.memory.read().await;
    if let Some(sub_id) = subscription_id {
        if mem.subscriptions.find_by_id(&sub_id).is_none() {
            return Err(format!("Subscription {} not found", sub_id));
        }
        if let Some(sub) = mem.subscriptions.find_by_id(&sub_id) {
            if !sub.is_active() {
                return Err(format!("Subscription is not active {}", sub_id));
            }
        }
    }

    let proto = match proto {
        // WG-family protocols share the allocation logic; each tag draws
        // from its own address pool.
        Tag::Wireguard | Tag::AmneziaWg | Tag::AmneziaWgMobile => {
            let network = match proto {
                Tag::Wireguard => wg_network,
                Tag::AmneziaWg => awg_network,
                Tag::AmneziaWgMobile => awg_mobile_network
                    .as_ref()
                    .ok_or("amnezia_wireguard_mobile_network is not configured")?,
                _ => unreachable!(),
            };

            let last_ip: Option<Ipv4Addr> = mem
                .connections
                .get_last_addr(proto)
                .and_then(|mask| mask.as_ipv4());

            let next = match last_ip {
                Some(ip) => IpAddrMask::increment_ipv4(ip),
                None => network.first_peer_ip(),
            };

            let next = next.ok_or("Failed to allocate IP")?;

            if !network.contains_ipv4(next) {
                return Err("IP out of range".to_string());
            }

            let param = WgParam {
                keys: WgKeys::default(),
                address: IpAddrMask {
                    address: IpAddr::V4(next),
                    cidr: 32,
                },
            };

            match proto {
                Tag::Wireguard => Proto::Wireguard { param },
                Tag::AmneziaWg => Proto::AmneziaWg { param },
                _ => Proto::AmneziaWgMobile { param },
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
        | Tag::Vmess => Proto::Xray(proto),
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

    let conn: Connection = Connection::new(env, subscription_id, proto, expired_at);
    let conn_id = uuid::Uuid::new_v4();
    let msg = conn.as_create_message(&conn_id);

    let messages = vec![msg];

    match SyncOp::add_conn(memory, &conn_id, conn.clone(), label, node_id).await {
        Ok(Status::Ok(id)) => {
            let bytes = match rkyv::to_bytes::<_, 1024>(&messages) {
                Ok(b) => b,
                Err(e) => return Err(format!("Serialization error: {}", e)),
            };

            // A pinned conn exists only on its node; since a node treats
            // Action::Create and Action::Update identically, the create
            // must go to the pin's Init topic, not the env broadcast.
            let topic = if conn.get_token().is_some() {
                Some(Topic::Auth)
            } else if conn.get_proto().is_mtproto() {
                None
            } else {
                match node_id {
                    Some(pin) => Some(Topic::Init(pin)),
                    None => Some(conn.get_env().into()),
                }
            };

            if let Some(topic) = topic {
                let _ = memory.publisher.send_binary(&topic, bytes.as_ref()).await;
            }

            Ok((id, conn))
        }
        Ok(Status::AlreadyExist(id)) => Ok((id, conn)),
        Ok(Status::BadRequest(_, msg)) => Err(format!("BadRequest {} {}", conn_id, msg)),
        Ok(_) => Err("Unsupported operation status".to_string()),
        Err(err) => Err(format!(
            "Internal error while processing connection {}: {}",
            conn_id, err
        )),
    }
}

/// (env, tag) pairs already covered by the subscription's *default*
/// (unlabeled) connections. Labeled connections are user-named devices:
/// a named "Мама Wireguard" must not count as "a default WG connection
/// already exists" when the renewal/activation top-up runs.
pub(crate) fn existing_default_pairs(
    conns: &[(uuid::Uuid, fcore::Env, Tag)],
    labels: &std::collections::HashMap<uuid::Uuid, String>,
) -> std::collections::HashSet<(fcore::Env, Tag)> {
    conns
        .iter()
        .filter(|(conn_id, _, _)| !labels.contains_key(conn_id))
        .map(|(_, env, tag)| (env.clone(), *tag))
        .collect()
}

/// Validation for a node pin on POST /connection: the node must exist,
/// belong to the connection's env, and expose the requested protocol's
/// inbound.
pub(crate) fn validate_node_pin(
    node: Option<&fcore::Node>,
    env: &fcore::Env,
    proto: Tag,
) -> Result<(), String> {
    let Some(node) = node else {
        return Err("Node not found".to_string());
    };
    if node.env != *env {
        return Err("Node env mismatch".to_string());
    }
    if !node.inbounds.values().any(|i| i.tag == proto) {
        return Err(format!("Node has no {} inbound", proto));
    }
    Ok(())
}

/// Sync catch-up visibility for node-pinned connections. A node asking on
/// its own Init topic sees unpinned conns and conns pinned to it; pinned
/// conns never leak to any other topic (env-wide Updates, Auth, mgmt).
pub(crate) fn sync_conn_visible(pin: Option<&uuid::Uuid>, topic: &Topic) -> bool {
    match (pin, topic) {
        (Some(pin), Topic::Init(u)) => pin == u,
        (Some(_), _) => false,
        (None, _) => true,
    }
}

/// Listing visibility for node-pinned connections: a pinned conn appears
/// only on the node whose uuid matches its pin; unpinned conns appear on
/// every node. Returns true = the conn is visible on this node.
pub(crate) fn pinned_to(
    conn_nodes: &std::collections::HashMap<uuid::Uuid, uuid::Uuid>,
    conn_id: &uuid::Uuid,
    node_uuid: &uuid::Uuid,
) -> bool {
    match conn_nodes.get(conn_id) {
        Some(pin) => pin == node_uuid,
        None => true,
    }
}

/// Ensure the subscription has a connection for every (env, tag) pair from
/// enabled_conns. Any existing *default* (unlabeled) connection of the
/// subscription — including soft-deleted ones, which the restore flow
/// revives on renewal — counts as present, so only genuinely missing pairs
/// are created. Labeled (named-device) connections are ignored: they are
/// extras on top of the defaults, not replacements. Errors are logged,
/// never propagated: this is a best-effort top-up on renewal/activation.
pub async fn ensure_enabled_connections<N, C, S>(
    subscription_id: uuid::Uuid,
    enabled_conns: &Option<std::collections::HashMap<fcore::Env, Vec<Tag>>>,
    memory: &MemSync<N, C, S>,
    wg_network: &IpAddrMask,
    awg_network: &IpAddrMask,
    awg_mobile_network: &Option<IpAddrMask>,
) where
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
    let Some(conns_map) = enabled_conns else {
        return;
    };

    let existing: std::collections::HashSet<(fcore::Env, Tag)> = {
        let mem = memory.memory.read().await;
        let conns: Vec<(uuid::Uuid, fcore::Env, Tag)> = mem
            .connections
            .get_by_subscription_id(&subscription_id)
            .unwrap_or_default()
            .iter()
            .map(|(conn_id, conn)| (*conn_id, conn.get_env(), conn.get_proto().proto()))
            .collect();
        existing_default_pairs(&conns, &mem.conn_labels)
    };

    for (env, tags) in conns_map {
        for tag in tags {
            if existing.contains(&(env.clone(), *tag)) {
                continue;
            }

            if let Err(err) = create_connection_inner(
                env,
                *tag,
                Some(subscription_id),
                None,
                None,
                None,
                memory,
                wg_network,
                awg_network,
                awg_mobile_network,
            )
            .await
            {
                error!(
                    "Failed to ensure connection for sub {} env {:?} tag {:?}: {}",
                    subscription_id, env, tag, err
                );
            }
        }
    }
}

/// Handler creates connection
// POST /connection
pub async fn create_connection_handler<N, C, S>(
    conn_req: ConnCreateRequest,
    memory: MemSync<N, C, S>,
    wg_network: IpAddrMask,
    awg_network: IpAddrMask,
    awg_mobile_network: Option<IpAddrMask>,
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

    // A pin must name a node of the requested env that actually exposes
    // the requested protocol's inbound.
    if let Some(node_id) = conn_req.node_id {
        let mem = memory.memory.read().await;
        let node = mem.nodes.get_by_id(&node_id);
        if let Err(msg) = validate_node_pin(node.as_ref(), &conn_req.env, conn_req.proto) {
            return Ok(http::bad_request(&msg));
        }
    }

    match create_connection_inner(
        &conn_req.env,
        conn_req.proto,
        conn_req.subscription_id,
        conn_req.days,
        conn_req.normalized_label(),
        conn_req.node_id,
        &memory,
        &wg_network,
        &awg_network,
        &awg_mobile_network,
    )
    .await
    {
        Ok((id, conn)) => Ok(http::success_response(
            format!("Connection {} has been created", id),
            Some(id),
            Instance::Connection(conn),
        )),
        Err(msg) => {
            if msg.contains("not found") || msg.contains("not active") || msg.contains("IP out of range") {
                Ok(http::bad_request(&msg))
            } else {
                Ok(http::internal_error(&msg))
            }
        }
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

            // Named-device scope (conn=<connection_id>) and share-child
            // exclusion: child connections issued via share tokens must not
            // duplicate the owner's configs here.
            if req.conn.is_some_and(|cid| cid != conn_id) || mem.share_conns.contains(&conn_id) {
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
                    // A node-pinned conn exists only on its node.
                    if !pinned_to(&mem.conn_nodes, &conn_id, &node.uuid) {
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
                                "conn_label": mem.conn_labels.get(&conn_id),
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

            // Same scope/exclusion as the WG handler above.
            if req.conn.is_some_and(|cid| cid != conn_id) || mem.share_conns.contains(&conn_id) {
                continue;
            }

            let conn_tag = conn.get_proto().proto();
            if !matches!(conn_tag, Tag::AmneziaWg | Tag::AmneziaWgMobile) {
                continue;
            }

            if let Some(nodes) = mem.nodes.get_by_env(&conn.get_env()) {
                for node in nodes {
                    if node.status != NodeStatus::Online {
                        continue;
                    }
                    // A node-pinned conn exists only on its node.
                    if !pinned_to(&mem.conn_nodes, &conn_id, &node.uuid) {
                        continue;
                    }
                    if let Some(inbound) = node.inbounds.get(&conn_tag) {
                        let c: Connection = conn.clone().into();
                        let host = node.connection_host();

                        if let Ok(link) =
                            inbound.create_link(&conn_id, &c, &node.hostname, &host, &node.label)
                        {
                            result.push(serde_json::json!({
                                "conn_id": conn_id,
                                "label": node.label,
                                "conn_label": mem.conn_labels.get(&conn_id),
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
        for (conn_id, conn) in conns {
            if conn.get_deleted() || conn.get_env() != req.env {
                continue;
            }

            // Same scope/exclusion as the WG handler above.
            if req.conn.is_some_and(|cid| cid != conn_id) || mem.share_conns.contains(&conn_id) {
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
                    // A node-pinned conn exists only on its node.
                    if !pinned_to(&mem.conn_nodes, &conn_id, &node.uuid) {
                        continue;
                    }
                    if let Some(inbound) = node.inbounds.get(&Tag::Mtproto) {
                        let host = node.connection_host();
                        let link = inbound.mtproto(&node.hostname, &host, &node.label);

                        if let Ok(url) = link {
                            result.push(serde_json::json!({
                                "label": node.label,
                                "conn_label": mem.conn_labels.get(&conn_id),
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


#[cfg(test)]
mod tests {
    use super::{existing_default_pairs, pinned_to, sync_conn_visible, validate_node_pin};
    use chrono::Utc;
    use fcore::{Env, Inbound, Node, NodeStatus, NodeType, Tag, Topic};
    use std::collections::HashMap;

    #[test]
    fn test_existing_default_pairs_ignores_labeled() {
        let default_wg = uuid::Uuid::new_v4();
        let labeled_awg = uuid::Uuid::new_v4();
        let labeled_h2 = uuid::Uuid::new_v4();

        let conns = vec![
            (default_wg, Env::Ru, Tag::Wireguard),
            (labeled_awg, Env::Ru, Tag::AmneziaWg),
            (labeled_h2, Env::Ru, Tag::Hysteria2),
        ];

        let labels: HashMap<uuid::Uuid, String> = [
            (labeled_awg, "Мама Андроид".to_string()),
            (labeled_h2, "Мама H2".to_string()),
        ]
        .into_iter()
        .collect();

        let pairs = existing_default_pairs(&conns, &labels);

        // Only the unlabeled default counts: the labeled AWG/H2 devices do
        // not cover their (env, tag) pairs, so the top-up would still
        // create default connections for them.
        assert!(pairs.contains(&(Env::Ru, Tag::Wireguard)));
        assert!(!pairs.contains(&(Env::Ru, Tag::AmneziaWg)));
        assert!(!pairs.contains(&(Env::Ru, Tag::Hysteria2)));
        assert_eq!(pairs.len(), 1);
    }

    #[test]
    fn test_existing_default_pairs_no_labels() {
        let id = uuid::Uuid::new_v4();
        let conns = vec![(id, Env::Dev, Tag::Mtproto)];

        let pairs = existing_default_pairs(&conns, &HashMap::new());

        assert!(pairs.contains(&(Env::Dev, Tag::Mtproto)));
        assert_eq!(pairs.len(), 1);
    }

    #[test]
    fn test_sync_conn_visible() {
        let node_a = uuid::Uuid::new_v4();
        let node_b = uuid::Uuid::new_v4();

        // Unpinned conns are visible on every topic (current behavior).
        assert!(sync_conn_visible(None, &Topic::Init(node_a)));
        assert!(sync_conn_visible(None, &Topic::Updates(Env::Ru)));
        assert!(sync_conn_visible(None, &Topic::Auth));

        // A pinned conn is visible only on its own node's Init topic.
        assert!(sync_conn_visible(Some(&node_a), &Topic::Init(node_a)));
        assert!(!sync_conn_visible(Some(&node_a), &Topic::Init(node_b)));
        assert!(!sync_conn_visible(Some(&node_a), &Topic::Updates(Env::Ru)));
        assert!(!sync_conn_visible(Some(&node_a), &Topic::Auth));
    }

    #[test]
    fn test_pinned_to() {
        let conn_id = uuid::Uuid::new_v4();
        let node_a = uuid::Uuid::new_v4();
        let node_b = uuid::Uuid::new_v4();

        // Unpinned: visible on every node.
        let conn_nodes: HashMap<uuid::Uuid, uuid::Uuid> = HashMap::new();
        assert!(pinned_to(&conn_nodes, &conn_id, &node_a));
        assert!(pinned_to(&conn_nodes, &conn_id, &node_b));

        // Pinned: visible only on the pin.
        let conn_nodes: HashMap<uuid::Uuid, uuid::Uuid> =
            [(conn_id, node_a)].into_iter().collect();
        assert!(pinned_to(&conn_nodes, &conn_id, &node_a));
        assert!(!pinned_to(&conn_nodes, &conn_id, &node_b));
    }

    fn test_node(env: Env, inbounds: HashMap<Tag, Inbound>) -> Node {
        Node {
            uuid: uuid::Uuid::new_v4(),
            env,
            hostname: "test-node".to_string(),
            address: "192.168.1.100".parse().unwrap(),
            status: NodeStatus::Online,
            label: "Test".to_string(),
            interface: "eth0".to_string(),
            created_at: Utc::now(),
            modified_at: Utc::now(),
            inbounds,
            cores: 4,
            max_bandwidth_bps: 1_000_000_000,
            country: "RU".to_string(),
            r#type: NodeType::Node,
            cluster: None,
        }
    }

    fn wg_inbound() -> Inbound {
        Inbound {
            tag: Tag::Wireguard,
            port: 51820,
            stream_settings: None,
            wg: None,
            awg: None,
            h2: None,
            mtproto_secret: None,
        }
    }

    #[test]
    fn test_validate_node_pin() {
        let mut inbounds = HashMap::new();
        inbounds.insert(Tag::Wireguard, wg_inbound());
        let node = test_node(Env::Ru, inbounds);

        // Happy path.
        assert!(validate_node_pin(Some(&node), &Env::Ru, Tag::Wireguard).is_ok());

        // Unknown node.
        assert_eq!(
            validate_node_pin(None, &Env::Ru, Tag::Wireguard),
            Err("Node not found".to_string())
        );

        // Wrong env.
        assert_eq!(
            validate_node_pin(Some(&node), &Env::Dev, Tag::Wireguard),
            Err("Node env mismatch".to_string())
        );

        // Node has no inbound for the requested protocol.
        assert_eq!(
            validate_node_pin(Some(&node), &Env::Ru, Tag::VlessTcpReality),
            Err("Node has no VlessTcpReality inbound".to_string())
        );
    }
}
