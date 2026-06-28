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
    pub bonus_days: i64,
    pub system_refer_codes: Vec<String>,
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
    pub fn new(
        memory: Arc<RwLock<Cache<N, C, S>>>,
        db: PgContext,
        publisher: Publisher,
        bonus_days: i64,
        system_refer_codes: Vec<String>,
    ) -> Self {
        Self {
            memory,
            db,
            publisher,
            bonus_days,
            system_refer_codes,
        }
    }
}
