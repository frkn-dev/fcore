use serde::Serialize;
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
/// Правила для `node_`-токена при регистрации ноды.
///
/// Токен лежит на машине self-hosted пользователя, поэтому он не даёт прав на
/// общие пространства: только своя нода, только персональный env и только тот
/// env, что у ноды уже был. Без этого обычный пользовательский токен
/// регистрировал ноду в `dev` и получал чужих пиров.
pub(crate) fn node_token_allows(
    bound_uuid: Option<uuid::Uuid>,
    request_uuid: uuid::Uuid,
    request_env_personal: bool,
    existing_env: Option<&fcore::Env>,
    request_env: &fcore::Env,
) -> Result<(), &'static str> {
    if let Some(bound) = bound_uuid {
        if bound != request_uuid {
            return Err("node token bound to another node");
        }
    }
    if !request_env_personal {
        return Err("node token cannot register a shared env");
    }
    if let Some(existing) = existing_env {
        if existing != request_env {
            return Err("env mismatch for node token");
        }
    }
    Ok(())
}

pub async fn post_node_handler<N, C, S>(
    node_req: NodeRequest,
    auth_header: String,
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
    if let Some(token) = auth_header.strip_prefix("Bearer ") {
        if token.starts_with("node_") {
            let bound = memory
                .db
                .node_access_token()
                .find_by_token(token)
                .await
                .ok()
                .flatten();
            let existing_env = {
                let mem = memory.memory.read().await;
                mem.nodes.get_by_id(&node_req.uuid).map(|n| n.env.clone())
            };
            if let Err(reason) = node_token_allows(
                bound,
                node_req.uuid,
                node_req.env.is_personal(),
                existing_env.as_ref(),
                &node_req.env,
            ) {
                return Ok(warp::reply::with_status(
                    warp::reply::json(&ResponseMessage::<Option<IdResponse>> {
                        status: StatusCode::FORBIDDEN.as_u16(),
                        message: reason.to_string(),
                        response: None,
                    }),
                    StatusCode::FORBIDDEN,
                ));
            }
        }
    }

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

    let key_mismatch = {
        let mem = memory.memory.read().await;
        let peers = mem.nodes.get_by_env(&node.env).unwrap_or_default();
        pool_inbounds_conflict(&peers, &node)
    };
    if key_mismatch {
        return Ok(warp::reply::with_status(
            warp::reply::json(&ResponseMessage::<Option<IdResponse>> {
                status: StatusCode::CONFLICT.as_u16(),
                message: "pool_key_mismatch".to_string(),
                response: None,
            }),
            StatusCode::CONFLICT,
        ));
    }

    let status = SyncOp::add_node(&memory, &node_id, node.clone()).await;

    let result_response = match status {
        Ok(Status::Ok(id)) | Ok(Status::AlreadyExist(id)) | Ok(Status::NotModified(id)) => {
            let _ =
                SyncOp::update_node_status(&memory, &node_id, &node.env, NodeStatus::Online).await;

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
#[derive(Clone, Debug, Serialize)]
pub struct NodeStatusPublic {
    pub hostname: String,
    pub address: Ipv4Addr,
    pub label: String,
    pub country: String,
    pub status: NodeStatus,
    pub r#type: NodeType,
    pub ports: Vec<u16>,
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
        .into_iter()
        .filter(|node| !node.env.is_personal())
        .map(|node| {
            let res = node.as_node_response();
            let mut ports: Vec<u16> = res.inbounds.iter().map(|i| i.port).collect();
            ports.sort_unstable();
            ports.dedup();
            NodeStatusPublic {
                hostname: res.hostname,
                address: res.address,
                label: res.label,
                country: res.country,
                status: res.status,
                r#type: res.r#type,
                ports,
            }
        })
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
mod node_token_tests {
    use super::node_token_allows;
    use fcore::Env;

    fn personal(tag: &str) -> Env {
        // так же, как personal_env() в private.rs
        Env::Custom(format!("custompersonal{tag}"))
    }

    #[test]
    fn node_token_cannot_register_shared_env() {
        let id = uuid::Uuid::from_u128(1);
        // именно этот сценарий и случился: обычный self-hosted токен -> dev
        assert_eq!(
            node_token_allows(Some(id), id, false, None, &Env::Dev),
            Err("node token cannot register a shared env")
        );
    }

    #[test]
    fn node_token_cannot_claim_other_node() {
        let own = uuid::Uuid::from_u128(1);
        let other = uuid::Uuid::from_u128(2);
        assert_eq!(
            node_token_allows(Some(own), other, true, None, &personal("a")),
            Err("node token bound to another node")
        );
    }

    #[test]
    fn node_token_cannot_move_between_envs() {
        let id = uuid::Uuid::from_u128(1);
        let existing = personal("a");
        let requested = personal("b");
        assert_eq!(
            node_token_allows(Some(id), id, true, Some(&existing), &requested),
            Err("env mismatch for node token")
        );
    }

    #[test]
    fn own_personal_env_is_allowed() {
        let id = uuid::Uuid::from_u128(1);
        let env = personal("a");
        assert!(node_token_allows(Some(id), id, true, Some(&env), &env).is_ok());
        // первая регистрация: строки ещё нет — разрешаем, env зафиксируется
        assert!(node_token_allows(Some(id), id, true, None, &env).is_ok());
    }
}
