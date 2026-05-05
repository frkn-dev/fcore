use std::sync::Arc;
use tokio::sync::RwLock;

use fcore::{ConnectionBaseOperations, ConnectionStorageBaseOperations, Connections};

use super::request;
use super::response;

pub async fn auth_handler<C>(
    req: request::Auth,
    memory: Arc<RwLock<Connections<C>>>,
) -> Result<impl warp::Reply, warp::Rejection>
where
    C: ConnectionBaseOperations + Sync + Send + Clone + 'static + std::fmt::Display,
{
    tracing::debug!("Auth req {} {} {}", req.auth, req.addr, req.tx);
    let mem = memory.read().await;
    if let Some(id) = mem.validate_token(&req.auth) {
        Ok(warp::reply::json(&response::Auth {
            ok: true,
            id: Some(id.to_string()),
        }))
    } else {
        Ok(warp::reply::json(&response::Auth {
            ok: false,
            id: None,
        }))
    }
}
