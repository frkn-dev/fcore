pub mod connection;
pub mod key;
pub mod metrics;
pub mod node;
pub mod subscription;
pub mod trial;

use warp::http::StatusCode;

use fcore::{
    http::ResponseMessage, Connection, ConnectionApiOperations, ConnectionBaseOperations,
    NodeStorageOperations, SubscriptionOperations,
};

use crate::sync::MemSync;

// GET /healthcheck
pub async fn healthcheck_handler<N, C, S>(
    _state: MemSync<N, C, S>,
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
    let response = ResponseMessage::<Option<uuid::Uuid>> {
        status: 200,
        message: "Ok".to_string(),
        response: None,
    };

    Ok(warp::reply::with_status(
        warp::reply::json(&response),
        StatusCode::OK,
    ))
}
