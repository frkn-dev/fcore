use std::sync::Arc;
use warp::http::StatusCode;

use fcore::{
    http::{IdResponse, ResponseMessage},
    Connection, ConnectionApiOperations, ConnectionBaseOperations, MetricStorage, NodeMetricInfo,
    NodeResponse, NodeStatus, NodeStorageOperations, Status, Subscription, SubscriptionOperations,
};

use super::super::{
    super::sync::{tasks::SyncOp, MemSync},
    param::NodesQueryParams,
    request::NodeRequest,
};

// Register node handler
// POST /node
pub async fn post_node_handler<N, C, S>(
    node_req: NodeRequest,
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
    tracing::debug!("Received node request: {:?}", node_req);

    let node = node_req.clone().as_node();
    let node_id = node_req.uuid;

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
