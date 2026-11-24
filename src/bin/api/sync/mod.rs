use std::sync::Arc;
use tokio::sync::RwLock;

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
    /// Own-node quota granted to every new top-level subscription
    /// (`[service.private] default_own_nodes_limit`). 0 disables it. Child
    /// subscriptions never get it.
    pub own_nodes_default: i64,
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
            own_nodes_default: fcore::DEFAULT_OWN_NODES_LIMIT,
        }
    }

    /// Sets the own-node quota of new subscriptions from the api config.
    pub fn with_own_nodes_default(mut self, limit: i64) -> Self {
        self.own_nodes_default = limit.max(0);
        self
    }
}
