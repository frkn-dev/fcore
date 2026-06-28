use std::collections::BTreeMap;
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
    pub referral_bonus_tiers: BTreeMap<i64, i64>,
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
        referral_bonus: std::collections::HashMap<String, i64>,
        system_refer_codes: Vec<String>,
    ) -> Self {
        let referral_bonus_tiers = referral_bonus
            .into_iter()
            .filter_map(|(k, v)| k.parse::<i64>().ok().map(|threshold| (threshold, v)))
            .collect::<BTreeMap<i64, i64>>();

        Self {
            memory,
            db,
            publisher,
            referral_bonus_tiers,
            system_refer_codes,
        }
    }
}
