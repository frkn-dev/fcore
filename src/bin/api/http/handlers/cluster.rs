use warp::http::StatusCode;

use fcore::{
    http::ResponseMessage, Connection, ConnectionApiOperations, ConnectionBaseOperations,
    NodeResponse, NodeStorageOperations, Subscription, SubscriptionOperations,
};

use super::super::super::sync::MemSync;

pub async fn get_clusters_handler<N, C, S>(
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
    let clusters = mem.nodes.all_clusters();

    let response = ResponseMessage {
        status: StatusCode::OK.as_u16(),
        message: "List of clusters".to_string(),
        response: Some(clusters),
    };

    Ok(warp::reply::with_status(
        warp::reply::json(&response),
        StatusCode::OK,
    ))
}

pub async fn get_cluster_nodes_handler<N, C, S>(
    cluster: String,
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
    let nodes: Vec<NodeResponse> = mem
        .nodes
        .get_by_cluster(&cluster)
        .into_iter()
        .map(|node| node.as_node_response())
        .collect();

    let response = ResponseMessage {
        status: StatusCode::OK.as_u16(),
        message: format!("Nodes in cluster {}", cluster),
        response: Some(nodes),
    };

    Ok(warp::reply::with_status(
        warp::reply::json(&response),
        StatusCode::OK,
    ))
}
