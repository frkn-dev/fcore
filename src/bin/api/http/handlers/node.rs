use serde::Serialize;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use warp::http::StatusCode;

use fcore::{
    http::{IdResponse, ResponseMessage},
    Connection, ConnectionApiOperations, ConnectionBaseOperations, MetricStorage, NodeMetricInfo,
    NodeResponse, NodeStatus, NodeStorageOperations, NodeType, Status, Subscription,
    SubscriptionOperations,
};

use super::super::{
    super::sync::{tasks::SyncOp, MemSync},
    param::NodesQueryParams,
    request::{pool_inbounds_conflict, NodeRequest},
};

// Register node handler
// POST /node
pub async fn post_node_handler<N, C, S>(
    node_req: NodeRequest,
    memory: MemSync<N, C, S>,
    strict_inbounds: bool,
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
    // Сводка вместо `{:?}`: в запросе инбаунды с секретами (mtproto_secret),
    // а на debug-уровне это утечка в journalctl.
    tracing::debug!(
        node = %node_req.uuid,
        env = %node_req.env,
        hostname = %node_req.hostname,
        inbounds = node_req.inbounds.len(),
        "node registration"
    );

    if let Err(e) = node_req.validate() {
        return Ok(warp::reply::with_status(
            warp::reply::json(&ResponseMessage::<Option<IdResponse>> {
                status: StatusCode::BAD_REQUEST.as_u16(),
                message: e.to_string(),
                response: None,
            }),
            StatusCode::BAD_REQUEST,
        ));
    }

    // `node_`-токен живёт на машине self-hosted пользователя, поэтому он не
    // должен давать права на чужие пространства: только своя нода, только
    // персональный env и только тот env, который у ноды уже был. До этой
    // проверки обычный пользовательский токен регистрировал ноду в dev.
    if node_req.env.is_personal() {
        return Ok(warp::reply::with_status(
            warp::reply::json(&ResponseMessage::<Option<IdResponse>> {
                status: StatusCode::FORBIDDEN.as_u16(),
                message: "personal env requires /private/nodes".to_string(),
                response: None,
            }),
            StatusCode::FORBIDDEN,
        ));
    }

    let node = node_req.clone().as_node();
    let node_id = node_req.uuid;

    // A mismatch is a diagnosis, not a door lock: the node still registers and
    // still gets connections through /connections/sync. Blocking registration
    // used to leave the pool on a stale snapshot and silently stop handing new
    // configs to clients.
    let conflict = {
        let mem = memory.memory.read().await;
        let peers = mem.nodes.get_by_env(&node.env).unwrap_or_default();
        pool_inbounds_conflict(&peers, &node)
    };

    if let Some(conflict) = &conflict {
        // Field-level detail in the log: without it the only thing visible was
        // fnode's "409 Conflict" line, with no clue which tag or field differed.
        tracing::warn!(
            node = %node_req.hostname,
            pool = %conflict.pool,
            peer = %conflict.peer_hostname,
            differences = %serde_json::to_string(&conflict.differences)
                .unwrap_or_else(|_| "unserializable".to_string()),
            strict = strict_inbounds,
            "node inbounds do not match its pool"
        );
    }

    let status = SyncOp::add_node(&memory, &node_id, node.clone()).await;

    let result_response = match status {
        Ok(Status::Ok(id)) | Ok(Status::AlreadyExist(id)) | Ok(Status::NotModified(id)) => {
            let node_status = if conflict.is_some() {
                NodeStatus::Degraded
            } else {
                NodeStatus::Online
            };
            let _ = SyncOp::update_node_status(&memory, &node_id, &node.env, node_status).await;

            ResponseMessage::<Option<IdResponse>> {
                status: StatusCode::OK.as_u16(),
                message: "Ok".to_string(),
                response: Some(IdResponse { id }),
            }
        }

        Ok(Status::Updated(_))
        | Ok(Status::DeletedPreviously(_))
        | Ok(Status::BadRequest(_, _))
        | Ok(Status::UpdatedStat(_))
        | Ok(Status::NotFound(_)) => ResponseMessage::<Option<IdResponse>> {
            status: StatusCode::BAD_REQUEST.as_u16(),
            message: "Operation not supported".into(),
            response: None,
        },

        Err(e) => {
            tracing::error!("Error adding node: {} for {}", e, node_id);
            return Ok(warp::reply::with_status(
                warp::reply::json(&ResponseMessage::<Option<uuid::Uuid>> {
                    status: StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
                    message: format!("INTERNAL_SERVER_ERROR {} for {}", e, node_id),
                    response: None,
                }),
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    };

    // Strict mode keeps the old, loud behaviour for deliberate rollouts: the
    // caller sees a 409 *and* knows exactly what differs, while the data plane
    // stays up because the node was registered above.
    if let Some(conflict) = conflict {
        if strict_inbounds && result_response.status == StatusCode::OK.as_u16() {
            return Ok(warp::reply::with_status(
                warp::reply::json(&ResponseMessage {
                    status: StatusCode::CONFLICT.as_u16(),
                    message: "pool_inbounds_conflict".to_string(),
                    response: Some(conflict),
                }),
                StatusCode::CONFLICT,
            ));
        }
    }

    let status_code =
        StatusCode::from_u16(result_response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    Ok(warp::reply::with_status(
        warp::reply::json(&result_response),
        status_code,
    ))
}

/// List of nodes handler
pub async fn get_nodes_handler<N, C, S>(
    node_param: NodesQueryParams,
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static,
{
    let mem = memory.memory.read().await;

    let nodes = if let Some(env) = node_param.env {
        mem.nodes.get_by_env(&env.into())
    } else {
        mem.nodes.all()
    };

    match nodes {
        Some(nodes) => {
            let node_response: Vec<NodeResponse> = nodes
                .into_iter()
                .filter(|node| !node.env.is_personal())
                .map(|node| node.as_node_response())
                .collect();

            let response = ResponseMessage {
                status: StatusCode::OK.as_u16(),
                message: "List of nodes".to_string(),
                response: Some(node_response),
            };

            Ok(warp::reply::with_status(
                warp::reply::json(&response),
                StatusCode::OK,
            ))
        }
        None => {
            let response = ResponseMessage::<Option<Vec<NodeResponse>>> {
                status: StatusCode::NOT_FOUND.as_u16(),

                message: "List of nodes".to_string(),

                response: vec![].into(),
            };

            Ok(warp::reply::with_status(
                warp::reply::json(&response),
                StatusCode::NOT_FOUND,
            ))
        }
    }
}

/// Public node status: no inbounds, tags, UUIDs or metrics — only what the
/// status page needs to render and probe nodes. `ports` is a deduplicated
/// list of listen ports for connectivity checks, without protocol tags.
///
/// `pool` and `pool_translation` are passed through verbatim from the node's
/// config (`[node] pool` / `[node] pool_translation`): the pool name is the
/// identifier, the translation map is what a UI renders. Both are always
/// present in JSON (`null` when the node has none) so a consumer can tell
/// "legacy node without a pool" from "old api that does not know the field".
#[derive(Clone, Debug, Serialize)]
pub struct NodeStatusPublic {
    pub hostname: String,
    pub address: Ipv4Addr,
    pub label: String,
    pub country: String,
    pub status: NodeStatus,
    pub r#type: NodeType,
    pub ports: Vec<u16>,
    pub pool: Option<String>,
    pub pool_translation: Option<BTreeMap<String, String>>,
    /// Base URL of the node's own speed-test endpoint, verbatim from
    /// `nodes.bench_url`; `null` when the node has no bench (premium slots).
    /// Consumers postfix `/speedtest/{1mb.test,10mb.test,ping.txt}` themselves.
    pub speedtest_url: Option<String>,
}

/// Is this node listed on the public status page? Personal (self-hosted)
/// nodes are never published: they belong to their owner, not to FRKN.
fn status_listed(node: &fcore::Node) -> bool {
    !node.env.is_personal()
}

/// Map a node into its public status row (ports deduplicated, pool verbatim).
fn node_status_public(node: &fcore::Node) -> NodeStatusPublic {
    let mut ports: Vec<u16> = node.inbounds.values().map(|i| i.port).collect();
    ports.sort_unstable();
    ports.dedup();
    NodeStatusPublic {
        hostname: node.hostname.clone(),
        address: node.address,
        label: node.label.clone(),
        country: node.country.clone(),
        status: node.status.clone(),
        r#type: node.r#type.clone(),
        ports,
        pool: node.pool.clone(),
        pool_translation: node.pool_translation.clone(),
        speedtest_url: node.bench_url.clone(),
    }
}

/// Public list of node statuses handler
// GET /status
pub async fn get_nodes_status_handler<N, C, S>(
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static,
{
    let mem = memory.memory.read().await;

    let nodes: Vec<NodeStatusPublic> = mem
        .nodes
        .all()
        .unwrap_or_default()
        .iter()
        .filter(|node| status_listed(node))
        .map(node_status_public)
        .collect();

    let response = ResponseMessage {
        status: StatusCode::OK.as_u16(),
        message: "List of node statuses".to_string(),
        response: Some(nodes),
    };

    Ok(warp::reply::with_status(
        warp::reply::json(&response),
        StatusCode::OK,
    ))
}

/// Get single node handler
pub async fn get_node_handler<N, C, S>(
    node_id: uuid::Uuid,
    memory: MemSync<N, C, S>,
    metrics: Arc<MetricStorage>,
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static,
{
    let mem = memory.memory.read().await;

    if let Some(node) = mem.nodes.get_by_id(&node_id) {
        if node.env.is_personal() {
            let response = ResponseMessage::<Option<NodeResponse>> {
                status: StatusCode::NOT_FOUND.as_u16(),
                message: "Node not found".to_string(),
                response: None,
            };
            return Ok(warp::reply::with_status(
                warp::reply::json(&response),
                StatusCode::NOT_FOUND,
            ));
        }
        let mut res = node.as_node_response();

        res.metrics = if let Some(node_metrics_map) = metrics.inner.get(&node_id) {
            node_metrics_map
                .iter()
                .filter_map(|entry| {
                    let series_hash = entry.key();
                    let points = entry.value();

                    if points.is_empty() {
                        return None;
                    }

                    let (name, tags) = match metrics.metadata.get(series_hash) {
                        Some(meta) => {
                            let val = meta.value();
                            (val.0.clone(), val.1.clone())
                        }
                        None => return None,
                    };

                    if matches!(
                        name.as_str(),
                        "user.traffic.downlink" | "user.traffic.uplink" | "user.traffic.online"
                    ) {
                        return None;
                    }

                    Some(NodeMetricInfo {
                        key: series_hash.to_string(),
                        name,
                        tags,
                    })
                })
                .collect()
        } else {
            vec![]
        };

        tracing::info!("Node {} has {} metrics", node_id, res.metrics.len());

        let response = ResponseMessage::<Option<NodeResponse>> {
            status: StatusCode::OK.as_u16(),
            message: format!("Node {} with {} metrics", node_id, res.metrics.len()),
            response: Some(res),
        };

        Ok(warp::reply::with_status(
            warp::reply::json(&response),
            StatusCode::OK,
        ))
    } else {
        let response = ResponseMessage::<Option<NodeResponse>> {
            status: StatusCode::NOT_FOUND.as_u16(),
            message: "Node not found".to_string(),
            response: None,
        };
        Ok(warp::reply::with_status(
            warp::reply::json(&response),
            StatusCode::NOT_FOUND,
        ))
    }
}

/// Delete node handler
// DELETE /node/<uuid>
pub async fn delete_node_handler<N, C, S>(
    node_id: uuid::Uuid,
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
    let status = match SyncOp::delete_node(&memory, &node_id).await {
        Ok(Status::Ok(id)) => ResponseMessage::<Option<uuid::Uuid>> {
            status: StatusCode::OK.as_u16(),
            message: "Node deleted".to_string(),
            response: Some(id),
        },
        Ok(Status::NotFound(_)) => ResponseMessage::<Option<uuid::Uuid>> {
            status: StatusCode::NOT_FOUND.as_u16(),
            message: "Node not found".to_string(),
            response: None,
        },
        Ok(other) => ResponseMessage::<Option<uuid::Uuid>> {
            status: StatusCode::BAD_REQUEST.as_u16(),
            message: format!("Unexpected status: {:?}", other),
            response: None,
        },
        Err(e) => {
            tracing::error!("Error deleting node {}: {}", node_id, e);
            ResponseMessage::<Option<uuid::Uuid>> {
                status: StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
                message: format!("Failed to delete node: {}", e),
                response: None,
            }
        }
    };

    let status_code =
        StatusCode::from_u16(status.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    Ok(warp::reply::with_status(
        warp::reply::json(&status),
        status_code,
    ))
}

#[cfg(test)]
mod tests {
    use super::{node_status_public, status_listed};
    use fcore::{Env, Node, NodeStatus, NodeType};
    use std::collections::{BTreeMap, HashMap};

    fn node(env: Env, pool: Option<&str>, translation: Option<(&str, &str)>) -> Node {
        let translation: Option<BTreeMap<String, String>> = translation.map(|(lang, title)| {
            [(lang.to_string(), title.to_string())]
                .into_iter()
                .collect()
        });
        Node {
            uuid: uuid::Uuid::new_v4(),
            env,
            hostname: "eu01.frkn.org".to_string(),
            address: "185.156.108.232".parse().unwrap(),
            status: NodeStatus::Online,
            label: "NL AWG EU-01".to_string(),
            interface: "eth0".to_string(),
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
            inbounds: HashMap::new(),
            cores: 4,
            max_bandwidth_bps: 1_000_000_000,
            country: "EU".to_string(),
            r#type: NodeType::Node,
            cluster: None,
            node_ips: None,
            pool: pool.map(str::to_string),
            pool_translation: translation,
            node_weights: BTreeMap::new(),
            bench_url: None,
        }
    }

    fn with_bench(mut n: Node, url: &str) -> Node {
        n.bench_url = Some(url.to_string());
        n
    }

    // /status: the pool is passed through verbatim, together with its
    // translations; a node without a pool reports null, never a missing key.
    #[test]
    fn status_row_carries_pool_and_translation() {
        let n = node(Env::Dev, Some("EU Рилзопровод"), Some(("ru", "EU Рилзопровод")));
        let row = node_status_public(&n);
        assert_eq!(row.pool.as_deref(), Some("EU Рилзопровод"));
        assert_eq!(
            row.pool_translation
                .as_ref()
                .and_then(|t| t.get("ru"))
                .map(String::as_str),
            Some("EU Рилзопровод")
        );

        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["pool"], "EU Рилзопровод");
        assert_eq!(json["pool_translation"]["ru"], "EU Рилзопровод");
    }

    #[test]
    fn status_row_without_pool_is_null_not_missing() {
        let n = node(Env::Dev, None, None);
        let json = serde_json::to_value(node_status_public(&n)).unwrap();
        assert!(json.get("pool").is_some(), "pool key must always be present");
        assert!(json["pool"].is_null());
        assert!(json.get("pool_translation").is_some());
        assert!(json["pool_translation"].is_null());
    }

    // /status: the bench URL is passed through verbatim so consumers never have
    // to derive a hostname from the node name (the zone is
    // <hoster>-<cc>-<city>-bench-<nn>.bench.beacon.frkn.org now).
    #[test]
    fn status_row_carries_speedtest_url_verbatim() {
        let n = with_bench(
            node(Env::Dev, Some("EU Рилзопровод"), None),
            "https://ncr-nl-ams-bench-01.bench.beacon.frkn.org:19443",
        );
        let json = serde_json::to_value(node_status_public(&n)).unwrap();
        assert_eq!(
            json["speedtest_url"],
            "https://ncr-nl-ams-bench-01.bench.beacon.frkn.org:19443"
        );
        // no trailing slash and no path: the consumer adds /speedtest/...
        let url = json["speedtest_url"].as_str().unwrap();
        assert!(!url.ends_with('/'));
        assert!(!url.contains("/speedtest"));
    }

    // A node without a bench (premium slots) reports null, never a missing key.
    #[test]
    fn status_row_without_bench_is_null_not_missing() {
        let n = node(Env::Dev, Some("EU Рилзопровод"), None);
        let json = serde_json::to_value(node_status_public(&n)).unwrap();
        assert!(json.get("speedtest_url").is_some());
        assert!(json["speedtest_url"].is_null());
    }

    // Personal (self-hosted) nodes stay out of the public list.
    #[test]
    fn personal_nodes_are_not_listed() {
        let shared = node(Env::Dev, Some("EU Рилзопровод"), None);
        assert!(status_listed(&shared));
        let personal = node(
            Env::personal_for(uuid::Uuid::new_v4()),
            Some("EU Рилзопровод"),
            None,
        );
        assert!(!status_listed(&personal));
    }
}
