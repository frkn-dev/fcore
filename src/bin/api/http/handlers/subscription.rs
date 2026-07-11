use base64::Engine;
use chrono::{DateTime, Utc};
use tracing::Instrument;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use warp::http::{Response, StatusCode};

use fcore::http::{
    helpers as http,
    response::{
        EnvInfo, EnvTrafficHistoryBucket, EnvTrafficInfo, Instance, SubscriptionResponse,
        SubscriptionTrafficHistoryResponse, TrafficHistoryBucket,
    },
    ResponseMessage,
};

use fcore::{
    utils::get_uuid_last_octet_simple, Connection, ConnectionApiOperations,
    ConnectionBaseOperations, ConnectionStorageApiOperations, Env, Inbound, InboundClashConfig,
    InboundConnLink, MetricStorage, NodeStorageOperations, Status, Subscription,
    SubscriptionOperations, SubscriptionStorageOperations, Tag,
};

use super::super::super::{
    subscription_audit,
    sync::{tasks::SyncOp, MemSync},
};
use super::super::{
    param::SubIdQueryParam,
    request::EnvFilter,
    request::{FormatReq, Subscription as SubReq, SubscriptionInfoRequest},
};

#[derive(Debug, Deserialize)]
pub struct TrafficHistoryQuery {
    #[serde(default = "default_traffic_period")]
    pub period: String,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

fn default_traffic_period() -> String {
    "day".to_string()
}

use crate::traffic::{self, SubscriptionTraffic, TrafficValue};

pub(crate) async fn build_subscription_traffic(
    db: &crate::postgres::pg::PgContext,
    metrics: &MetricStorage,
    sub_id: uuid::Uuid,
    created_at: DateTime<Utc>,
) -> fcore::Result<SubscriptionTraffic> {
    let mut result = SubscriptionTraffic::default();
    let now = Utc::now();
    let day_bucket = traffic::day_start(now);
    let month_bucket = traffic::monthly_anchor(created_at, now);

    // Persisted lifetime total.
    let (total_up, total_down) = db.traffic().total_for_subscription(sub_id).await?;
    result.total = TrafficValue {
        uplink: total_up.max(0) as u64,
        downlink: total_down.max(0) as u64,
    };

    // Env-level persisted totals.
    let env_totals = db.traffic().env_totals_for_subscription(sub_id).await?;
    let daily_rows = db
        .traffic()
        .env_breakdown(sub_id, "day", day_bucket)
        .await?;
    let monthly_rows = db
        .traffic()
        .env_breakdown(sub_id, "month", month_bucket)
        .await?;

    let mut daily_map: HashMap<String, (i64, i64)> = HashMap::new();
    for (env, up, down) in daily_rows {
        daily_map.insert(env, (up, down));
    }
    let mut monthly_map: HashMap<String, (i64, i64)> = HashMap::new();
    for (env, up, down) in monthly_rows {
        monthly_map.insert(env, (up, down));
    }

    for (env, up, down) in env_totals {
        let (daily_up, daily_down) = daily_map.remove(&env).unwrap_or((0, 0));
        let (monthly_up, monthly_down) = monthly_map.remove(&env).unwrap_or((0, 0));
        result.add_persisted(
            &env,
            TrafficValue {
                uplink: up.max(0) as u64,
                downlink: down.max(0) as u64,
            },
            TrafficValue {
                uplink: daily_up.max(0) as u64,
                downlink: daily_down.max(0) as u64,
            },
            TrafficValue {
                uplink: monthly_up.max(0) as u64,
                downlink: monthly_down.max(0) as u64,
            },
        );
    }

    // Any env with only daily/monthly rows and no total rows.
    for (env, (daily_up, daily_down)) in daily_map {
        let (monthly_up, monthly_down) = monthly_map.remove(&env).unwrap_or((0, 0));
        result.add_persisted(
            &env,
            TrafficValue::default(),
            TrafficValue {
                uplink: daily_up.max(0) as u64,
                downlink: daily_down.max(0) as u64,
            },
            TrafficValue {
                uplink: monthly_up.max(0) as u64,
                downlink: monthly_down.max(0) as u64,
            },
        );
    }
    for (env, (monthly_up, monthly_down)) in monthly_map {
        result.add_persisted(
            &env,
            TrafficValue::default(),
            TrafficValue::default(),
            TrafficValue {
                uplink: monthly_up.max(0) as u64,
                downlink: monthly_down.max(0) as u64,
            },
        );
    }

    // Live delta since the last persistence boundary.
    let watermarks = db.conn().watermarks_for_subscription(sub_id).await?;
    for wm in watermarks {
        let segments = traffic::connection_deltas_between(
            metrics,
            &wm.conn_id,
            created_at,
            wm.last_persist_at,
            now,
        );

        let mut conn_total = TrafficValue::default();
        for seg in &segments {
            result.add_live_segment(&wm.env, seg);
            conn_total += TrafficValue {
                uplink: seg.uplink,
                downlink: seg.downlink,
            };
        }

        result.total += conn_total;
        if let Some(env_acc) = result.by_env.get_mut(&wm.env) {
            env_acc.total += conn_total;
        } else if conn_total.uplink > 0 || conn_total.downlink > 0 {
            let mut env_acc = crate::traffic::EnvTraffic::default();
            env_acc.total += conn_total;
            result.by_env.insert(wm.env.clone(), env_acc);
        }
    }

    Ok(result)
}

/// Handler creates subscription
// POST /subscription
pub async fn post_subscription_handler<N, C, S>(
    req: SubReq,
    trace_id_header: Option<String>,
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
    let trace_id = subscription_audit::trace_id_from_header(trace_id_header);
    let sub_id = uuid::Uuid::new_v4();

    let ref_code = req
        .refer_code
        .unwrap_or_else(|| get_uuid_last_octet_simple(&sub_id));

    let expires_at: Option<DateTime<Utc>> = req
        .days
        .map(|days| Utc::now() + chrono::Duration::days(days));

    let sub = Subscription::new(
        sub_id,
        ref_code,
        expires_at,
        req.limit_bytes,
    );

    subscription_audit::log_transaction_start(sub_id, req.days);

    match SyncOp::add_sub(&memory, sub.clone())
        .instrument(subscription_audit::transaction_span(
            "create_subscription_handler",
            sub_id,
            Some(trace_id),
        ))
        .await
    {
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
    trace_id_header: Option<String>,
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
    let trace_id = subscription_audit::trace_id_from_header(trace_id_header);
    let sub_id = sub_param.id;

    subscription_audit::log_transaction_start(sub_id, req.days);

    match SyncOp::update_sub(
        &memory,
        &sub_id,
        req,
    )
    .instrument(subscription_audit::transaction_span(
        "put_subscription_handler",
        sub_id,
        Some(trace_id),
    ))
    .await
    {
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
    let created_at = sub.created_at();
    let sub_id = sub.id();
    let expires = sub.expires_at().unwrap_or_default();
    let days = sub.days_remaining().unwrap_or(0);
    let ref_code = sub.refer_code();
    drop(mem);

    let traffic =
        match build_subscription_traffic(&memory.db, &metrics, subscription_id, created_at).await {
            Ok(t) => t,
            Err(e) => {
                return Ok(Box::new(http::internal_error(&format!(
                    "Traffic aggregation failed: {}",
                    e
                ))));
            }
        };

    let mut env_traffic_map: HashMap<Env, EnvTrafficInfo> = traffic
        .by_env
        .into_iter()
        .map(|(env_str, env_traffic)| {
            let env = traffic::parse_env(&env_str);
            (
                env.clone(),
                EnvTrafficInfo {
                    env,
                    uplink: env_traffic.total.uplink as i64,
                    downlink: env_traffic.total.downlink as i64,
                    daily_uplink: env_traffic.daily.uplink as i64,
                    daily_downlink: env_traffic.daily.downlink as i64,
                    monthly_uplink: env_traffic.monthly.uplink as i64,
                    monthly_downlink: env_traffic.monthly.downlink as i64,
                },
            )
        })
        .collect();

    // Ensure every location from the subscription has an entry even if traffic is zero.
    for loc in &locations {
        env_traffic_map
            .entry(loc.env.clone())
            .or_insert_with(|| EnvTrafficInfo {
                env: loc.env.clone(),
                uplink: 0,
                downlink: 0,
                daily_uplink: 0,
                daily_downlink: 0,
                monthly_uplink: 0,
                monthly_downlink: 0,
            });
    }

    let mut env_traffic: Vec<EnvTrafficInfo> = env_traffic_map.into_values().collect();
    env_traffic.sort_by(|a, b| a.env.to_string().cmp(&b.env.to_string()));

    let sub_resp = SubscriptionResponse {
        id: sub_id,
        expires,
        days,
        ref_code,
        locations,
        downlink: traffic.total.downlink as i64,
        uplink: traffic.total.uplink as i64,
        daily_downlink: traffic.daily.downlink as i64,
        daily_uplink: traffic.daily.uplink as i64,
        monthly_downlink: traffic.monthly.downlink as i64,
        monthly_uplink: traffic.monthly.uplink as i64,
        limit_bytes,
        env_traffic,
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
    let created_at = sub.created_at();
    let sub_id = sub.id();
    let expires_at = sub.expires_at().map(|e| e.timestamp()).unwrap_or(0);
    let limit = sub.limit_bytes();
    drop(mem);

    let traffic = match build_subscription_traffic(&memory.db, &metrics, req.id, created_at).await {
        Ok(t) => t,
        Err(e) => {
            return Ok(Box::new(http::internal_error(&format!(
                "Traffic aggregation failed: {}",
                e
            ))));
        }
    };

    let upload = traffic.total.uplink;
    let download = traffic.total.downlink;

    let sub_url = format!("{}/subscription?id={}", base_url, sub_id);

    let meta = format!(
        "#profile-title: {}\n\
         #profile-update-interval: 1\n\
         #subscription-userinfo: upload={}; download={}; total={}; expire={}\n\
         #profile-web-page-url: {}\n\
         #support-url: {}\n",
        title,
        upload,
        download,
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

/// GET /subscription/<id>/traffic
/// Returns persisted traffic history for a subscription grouped by period bucket and env.
pub async fn get_subscription_traffic_history<N, C, S>(
    subscription_id: uuid::Uuid,
    query: TrafficHistoryQuery,
    memory: MemSync<N, C, S>,
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
    {
        let mem = memory.memory.read().await;
        if mem.subscriptions.find_by_id(&subscription_id).is_none() {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&"Subscription not found"),
                warp::http::StatusCode::NOT_FOUND,
            )));
        }
    }

    let period = match query.period.as_str() {
        "day" | "month" => query.period,
        _ => "day".to_string(),
    };

    let rows = match memory
        .db
        .traffic()
        .history(subscription_id, &period, query.from, query.to)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            return Ok(Box::new(http::internal_error(&format!(
                "Failed to load traffic history: {}",
                e
            ))));
        }
    };

    let mut buckets = Vec::new();
    let mut current_bucket: Option<DateTime<Utc>> = None;
    let mut current_envs: Vec<EnvTrafficHistoryBucket> = Vec::new();
    let mut current_up: i64 = 0;
    let mut current_down: i64 = 0;

    for (bucket, env_str, up, down) in rows {
        if current_bucket != Some(bucket) {
            if let Some(b) = current_bucket {
                buckets.push(TrafficHistoryBucket {
                    bucket: b,
                    uplink: current_up,
                    downlink: current_down,
                    envs: current_envs,
                });
            }
            current_bucket = Some(bucket);
            current_envs = Vec::new();
            current_up = 0;
            current_down = 0;
        }

        let up = up.max(0);
        let down = down.max(0);
        current_up += up;
        current_down += down;
        current_envs.push(EnvTrafficHistoryBucket {
            env: traffic::parse_env(&env_str),
            uplink: up,
            downlink: down,
        });
    }

    if let Some(b) = current_bucket {
        buckets.push(TrafficHistoryBucket {
            bucket: b,
            uplink: current_up,
            downlink: current_down,
            envs: current_envs,
        });
    }

    let response = SubscriptionTrafficHistoryResponse {
        subscription_id,
        period,
        buckets,
    };

    Ok(Box::new(warp::reply::json(&response)))
}
