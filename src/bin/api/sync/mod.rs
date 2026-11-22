use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use super::{postgres::pg::PgContext, Cache};
use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, NodeStorageOperations,
    Publisher, SubscriptionOperations,
};

pub(crate) mod tasks;

#[derive(Clone)]
pub struct MemSync<N, C, S>
where
    N: Send + Sync + Clone + 'static,
    C: Send + Sync + Clone + 'static,
    S: Send + Sync + Clone + 'static,
{
    pub memory: Arc<RwLock<Cache<N, C, S>>>,
    pub db: PgContext,
    pub publisher: Publisher,
    pub connection_write: Arc<Mutex<()>>,
}

impl<N, C, S> MemSync<N, C, S>
where
    N: NodeStorageOperations + Send + Sync + Clone + 'static,
    C: ConnectionBaseOperations
        + ConnectionApiOperations
        + Send
        + Sync
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + 'static,
{
    pub fn new(memory: Arc<RwLock<Cache<N, C, S>>>, db: PgContext, publisher: Publisher) -> Self {
        Self {
            memory,
            db,
            publisher,
            connection_write: Arc::new(Mutex::new(())),
        }
    }
}
