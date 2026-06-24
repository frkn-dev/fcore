use base64::Engine;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::sync::Arc;
use warp::http::{Response, StatusCode};

use fcore::http::{
    helpers as http,
    response::{EnvInfo, Instance, SubscriptionResponse},
    ResponseMessage,
};

use fcore::{
    utils::get_uuid_last_octet_simple, Connection, ConnectionApiOperations,
    ConnectionBaseOperations, ConnectionStorageApiOperations, Env, Inbound, InboundClashConfig,
    InboundConnLink, MetricStorage, NodeStorageOperations, Status, Subscription,
    SubscriptionOperations, SubscriptionStorageOperations, Tag,
};

use super::super::super::sync::{tasks::SyncOp, MemSync};
use super::super::{
    param::SubIdQueryParam,
    request::EnvFilter,
    request::{FormatReq, Subscription as SubReq, SubscriptionInfoRequest},
};

/// Handler creates subscription
// POST /subscription
pub async fn post_subscription_handler<N, C, S>(
    req: SubReq,
    memory: MemSync<N, C, S>,
    bonus: i64,
    system_refer_codes: Vec<String>,
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
    let sub_id = uuid::Uuid::new_v4();
    let mut bonus_days = 0;

    let ref_code = req
        .refer_code
        .unwrap_or_else(|| get_uuid_last_octet_simple(&sub_id));

    let sub_id_to_update = if let Some(ref_by) = req.referred_by.clone() {
        let mem = memory.memory.read().await;

        let is_system_code = system_refer_codes.iter().any(|c| c == &ref_by);
        let is_user_referral = !is_system_code;

        if let Some(sub) = mem.subscriptions.find_by_refer_code(&ref_by) {
            if is_user_referral {
                bonus_days = bonus;
            }
            Some(sub.id())
        } else {
            return Ok(http::bad_request("Refer code no found"));
        }
    } else {
        None
    };

    if let Some(id) = sub_id_to_update {
        if let Err(e) = SyncOp::add_days(&memory, &id, bonus).await {
            return Ok(http::internal_error(&format!(
                "Couldn't create subscription: {}",
                e
            )));
        }
    }

    let expires_at: Option<DateTime<Utc>> = req
        .days
        .map(|days| Utc::now() + chrono::Duration::days(days + bonus_days));

    let sub = Subscription::new(
        sub_id,
        req.referred_by,
        ref_code,
        expires_at,
        req.limit_bytes,
    );

    match SyncOp::add_sub(&memory, sub.clone()).await {
        Ok(Status::Ok(id)) => Ok(http::success_response(
            format!("Subscription {} has been created", id),
            Some(sub_id),
            Instance::Subscription(sub),
        )),
        Ok(Status::AlreadyExist(id)) => Ok(http::not_modified(&format!(
            "Subscription {} already exists",
            id
        ))),
        Ok(Status::NotFound(id)) => Ok(http::not_found(&format!(
            "Subscription {} is not found",
            id
        ))),
        Err(err) => Ok(http::internal_error(&format!(
            "Internal error while processing subscription {}: {}",
            sub_id, err
        ))),
        _ => Ok(http::not_modified("")),
    }
}

// Handler updates subscription
// PUT /subscription
pub async fn put_subscription_handler<N, C, S>(
    sub_param: SubIdQueryParam,
    req: SubReq,
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
    let sub_id = sub_param.id;

    match SyncOp::update_sub(&memory, &sub_id, req).await {
        Ok(Status::Updated(id)) => Ok(http::success_response(
            format!("Subscription {} has been updated", id),
            Some(sub_id),
            Instance::None,
        )),
        Ok(Status::NotFound(id)) => Ok(http::not_found(&format!(
            "Subscription {} is not found",
            id
        ))),
        Err(err) => {
            let response = ResponseMessage::<Option<uuid::Uuid>> {
                status: 500,
                message: format!(
                    "Internal error while processing subscription {}: {}",
                    sub_id, err
                ),
                response: None,
            };
            Ok(warp::reply::with_status(
                warp::reply::json(&response),
                StatusCode::INTERNAL_SERVER_ERROR,
            ))
        }
        _ => Ok(http::not_modified("")),
    }
}

///get subscription_info_json
pub async fn get_subscription_info_json<N, C, S>(
    subscription_id: uuid::Uuid,
    memory: MemSync<N, C, S>,
    metrics: std::sync::Arc<MetricStorage>,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + PartialEq,
{
    let mem = memory.memory.read().await;

    let Some(sub) = mem.subscriptions.find_by_id(&subscription_id) else {
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(&"Subscription not found"),
            warp::http::StatusCode::NOT_FOUND,
        )));
    };

    let connections = mem.connections.get_by_subscription_id(&subscription_id);
    let mut locations = Vec::new();
    let (uplink, downlink) = metrics.get_subscription_total_traffic(&subscription_id);

    let downlink_i64 = downlink as i64;
    let uplink_i64 = uplink as i64;
    if let Some(conns) = connections.clone() {
        let active_envs: HashSet<Env> = conns
            .iter()
            .filter(|(_, conn)| !conn.get_deleted())
            .map(|(_, conn)| conn.get_env())
            .collect();

        for env in active_envs {
            let mut has_xray = false;
            let mut has_h2 = false;
            let mut has_mtproto = false;
            let mut has_wg = false;
            let mut has_awg = false;

            let nodes = mem.nodes.get_by_env(&env);

            let xray_tags = [
                Tag::VlessGrpcReality,
                Tag::VlessTcpReality,
                Tag::VlessXhttpReality,
                Tag::VlessXhttpCdn,
                Tag::Vmess,
            ];

            let xray_nodes = nodes.clone();
            let xray_node_exists = xray_nodes.is_some_and(|ns| {
                ns.iter()
                    .any(|n| n.inbounds.values().any(|i| xray_tags.contains(&i.tag)))
            });

            let mtproto_nodes = nodes.clone();
            let mtproto_node_exist = mtproto_nodes.is_some_and(|ns| {
                ns.iter()
                    .any(|n| n.inbounds.values().any(|i| i.tag == Tag::Mtproto))
            });

            let wg_nodes = nodes.clone();
            let wg_node_exist = wg_nodes.is_some_and(|ns| {
                ns.iter()
                    .any(|n| n.inbounds.values().any(|i| i.tag == Tag::Wireguard))
            });

            let awg_nodes = nodes.clone();
            let awg_node_exist = awg_nodes.is_some_and(|ns| {
                ns.iter()
                    .any(|n| n.inbounds.values().any(|i| i.tag == Tag::AmneziaWg))
            });

            let h2_node_exists = nodes.is_some_and(|ns| {
                ns.iter()
                    .any(|n| n.inbounds.values().any(|i| i.tag == Tag::Hysteria2))
            });

            for (_, conn) in conns.clone() {
                if !conn.get_deleted() && conn.get_env() == env {
                    let proto = conn.get_proto().proto();
                    if xray_node_exists && xray_tags.contains(&proto) {
                        has_xray = true;
                    }
                    if h2_node_exists && proto == Tag::Hysteria2 {
                        has_h2 = true;
                    }

                    if wg_node_exist && proto == Tag::Wireguard {
                        has_wg = true;
                    }

                    if awg_node_exist && proto == Tag::AmneziaWg {
                        has_awg = true;
                    }

                    if mtproto_node_exist && proto == Tag::Mtproto {
                        has_mtproto = true;
                    }
                }
            }

            locations.push(EnvInfo {
                env,
                has_xray,
                has_h2,
                has_mtproto,
                has_wg,
                has_awg,
            });
        }
    }

    let limit_bytes = sub.limit_bytes().unwrap_or(0);

    let sub_resp = SubscriptionResponse {
        id: sub.id(),
        expires: sub.expires_at().unwrap_or_default(),
        days: sub.days_remaining().unwrap_or(0),
        ref_code: sub.refer_code(),
        invited_count: mem.subscriptions.count_invited_by(&sub.refer_code()),
        locations,
        downlink: downlink_i64,
        uplink: uplink_i64,
        limit_bytes,
    };

    Ok(Box::new(warp::reply::json(&sub_resp)))
}

pub async fn subscription_link_handler<N, C, S>(
    req: SubscriptionInfoRequest,
    memory: MemSync<N, C, S>,
    metrics: Arc<MetricStorage>,
    title: String,
    base_url: String,
    support_contact: String,
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
    Vec<(uuid::Uuid, fcore::Connection)>: FromIterator<(uuid::Uuid, C)>,
{
    // -------------------------
    // Validate request
    // -------------------------
    if let Err(e) = req.validate() {
        return Ok(Box::new(http::bad_request(&format!("Bad Request: {}", e))));
    }

    let mem = memory.memory.read().await;

    // -------------------------
    // Subscription lookup
    // -------------------------
    let sub = match mem.subscriptions.find_by_id(&req.id) {
        Some(sub) if sub.is_active() => sub,
        Some(_) => {
            return Ok(Box::new(http::not_found(&format!(
                "Subscription {} is expired",
                req.id
            ))));
        }
        None => {
            return Ok(Box::new(http::not_found(&format!(
                "Subscription {} not found",
                req.id
            ))));
        }
    };

    // -------------------------
    // Prepare filters
    // -------------------------
    let proto_tags = req.proto.tags();
    let env_filter = &req.env;

    // -------------------------
    // Pre-filter connections
    // -------------------------
    let conns: Vec<(uuid::Uuid, Connection)> = mem
        .connections
        .get_by_subscription_id(&req.id)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, conn)| {
            if conn.get_deleted() {
                return false;
            }

            match env_filter {
                EnvFilter::All => true,
                EnvFilter::Single(env) => conn.get_env() == *env,
            }
        })
        .collect();

    // -------------------------
    // Build inbound list
    // -------------------------
    let mut inbounds_list: Vec<(Inbound, uuid::Uuid, Connection, String, String, String)> =
        Vec::new();

    match env_filter {
        EnvFilter::All => {
            for (conn_id, conn) in &conns {
                let proto = conn.get_proto().proto();
                let env = conn.get_env();

                if !proto_tags.contains(&proto) {
                    continue;
                }

                let nodes = mem.nodes.get_by_env(&env).unwrap_or_default();

                for node in nodes {
                    if let Some(inbound) = node.inbounds.get(&proto) {
                        inbounds_list.push((
                            inbound.clone(),
                            *conn_id,
                            conn.clone(),
                            node.hostname.clone(),
                            node.connection_host(),
                            node.cluster.clone().unwrap_or(node.label.clone()),
                        ));
                    }
                }
            }
        }

        EnvFilter::Single(env) => {
            let nodes = mem.nodes.get_by_env(env).unwrap_or_default();

            for (conn_id, conn) in &conns {
                let proto = conn.get_proto().proto();

                if !proto_tags.contains(&proto) {
                    continue;
                }

                for node in &nodes {
                    if let Some(inbound) = node.inbounds.get(&proto) {
                        inbounds_list.push((
                            inbound.clone(),
                            *conn_id,
                            conn.clone(),
                            node.hostname.clone(),
                            node.connection_host(),
                            node.cluster.clone().unwrap_or(node.label.clone()),
                        ));
                    }
                }
            }
        }
    }

    // -------------------------
    // Deduplicate nodes that belong to the same cluster
    // -------------------------
    inbounds_list.sort_by(|a, b| {
        a.4.cmp(&b.4)
            .then_with(|| a.0.tag.to_string().cmp(&b.0.tag.to_string()))
            .then_with(|| a.1.cmp(&b.1))
    });
    inbounds_list.dedup_by(|a, b| a.4 == b.4 && a.0.tag == b.0.tag && a.1 == b.1);

    // -------------------------
    // Empty check
    // -------------------------
    if inbounds_list.is_empty() {
        return Ok(Box::new(http::not_found(&format!(
            "Nodes for subscription {} not found",
            req.id
        ))));
    }

    // -------------------------
    // Subscription metadata
    // -------------------------
    let expires_at = sub.expires_at().map(|e| e.timestamp()).unwrap_or(0);

    let traffic = metrics.get_subscription_total_traffic(&req.id);
    let limit = sub.limit_bytes();

    let sub_url = format!("{}/subscription?id={}", base_url, sub.id());

    let meta = format!(
        "#profile-title: {}\n\
         #profile-update-interval: 1\n\
         #subscription-userinfo: upload={}; download={}; total={}; expire={}\n\
         #profile-web-page-url: {}\n\
         #support-url: {}\n",
        title,
        traffic.0,
        traffic.1,
        limit.unwrap_or(0),
        expires_at,
        sub_url,
        support_contact
    );

    // -------------------------
    // Response generation
    // -------------------------
    match req.format {
        FormatReq::Txt => {
            let links: Result<Vec<_>, _> = inbounds_list
                .iter()
                .map(|(inbound, conn_id, conn, hostname, host, label)| {
                    inbound.create_link(conn_id, conn, hostname, host, label)
                })
                .collect();

            let body = format!("{}{}", meta, links?.join("\n"));

            Ok(Box::new(warp::reply::with_status(
                warp::reply::with_header(body, "Content-Type", "text/plain"),
                StatusCode::OK,
            )))
        }

        FormatReq::Base64 => {
            let links: Result<Vec<_>, _> = inbounds_list
                .iter()
                .map(|(inbound, conn_id, conn, hostname, host, label)| {
                    inbound.create_link(conn_id, conn, hostname, host, label)
                })
                .collect();

            let body = format!("{}{}", meta, links?.join("\n"));

            let encoded = base64::engine::general_purpose::STANDARD.encode(body);

            Ok(Box::new(warp::reply::with_status(
                warp::reply::with_header(encoded, "Content-Type", "text/base64"),
                StatusCode::OK,
            )))
        }

        FormatReq::Clash => {
            let proxies: Vec<_> = inbounds_list
                .iter()
                .filter_map(|(inbound, conn_id, _conn, hostname, host, label)| {
                    inbound.proxy(conn_id, hostname, host, label)
                })
                .collect();

            let clash_config = Inbound::clash(proxies);

            let yaml = serde_yaml::to_string(&clash_config)
                .unwrap_or_else(|_| "---\nerror: failed to serialize\n".into());

            let response = Response::builder()
                .header("Content-Type", "application/yaml")
                .status(StatusCode::OK)
                .body(yaml);

            Ok(Box::new(response))
        }
    }
}
