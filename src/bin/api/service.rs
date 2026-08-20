use openssl::pkey::PKey;
use openssl::pkey::Private;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, Connections, MetricStorage,
    NodeStorageOperations, Subscription, SubscriptionOperations, Subscriptions,
};

use super::{config::ServiceSettings, sync::MemSync};

pub struct Service<N, C, S>
where
    N: NodeStorageOperations + Send + Sync + Clone + 'static + Default,
    C: ConnectionBaseOperations
        + ConnectionApiOperations
        + Send
        + Sync
        + Clone
        + 'static
        + PartialEq
        + From<Connection>,
    S: SubscriptionOperations
        + Send
        + Sync
        + Clone
        + 'static
        + Default
        + From<Subscription>
        + PartialEq,
{
    pub sync: MemSync<N, C, S>,
    pub settings: ServiceSettings,
    pub metrics: Arc<MetricStorage>,
    pub agw_private_key: Option<Arc<PKey<Private>>>,
}

impl<N, C, S> Service<N, C, S>
where
    N: NodeStorageOperations + Send + Sync + Clone + 'static + Default,
    C: ConnectionBaseOperations
        + ConnectionApiOperations
        + Send
        + Sync
        + Clone
        + 'static
        + PartialEq
        + From<Connection>,
    S: SubscriptionOperations
        + Send
        + Sync
        + Clone
        + 'static
        + Default
        + From<Subscription>
        + PartialEq,
{
    pub fn new(
        sync: MemSync<N, C, S>,
        settings: ServiceSettings,
        metrics: Arc<MetricStorage>,
        agw_private_key: Option<Arc<PKey<Private>>>,
    ) -> Self {
        Self {
            sync,
            settings,
            metrics,
            agw_private_key,
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Cache<T, C, S>
where
    T: Send + Sync + Clone + 'static,
    C: Send + Sync + Clone + 'static,
    S: Send + Sync + Clone + 'static,
{
    pub connections: Connections<C>,
    pub subscriptions: Subscriptions<S>,
    pub nodes: T,
    /// User-facing labels of named connections ("devices"), keyed by
    /// conn_id. Loaded from the PG-only `connections.label` column (never
    /// part of the rkyv payload) and maintained on create/delete/full
    /// reload. Absent entry = system/default connection.
    pub conn_labels: std::collections::HashMap<uuid::Uuid, String>,
}

impl<T: Default, C, S: Default + PartialEq> Default for Cache<T, C, S>
where
    T: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Clone
        + Send
        + Sync
        + 'static
        + PartialEq,
    S: SubscriptionOperations + Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Default, C, S: Default + PartialEq> Cache<T, C, S>
where
    T: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Clone
        + Send
        + Sync
        + 'static
        + PartialEq,
    S: SubscriptionOperations + Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Cache {
            nodes: T::default(),
            connections: Connections::default(),
            subscriptions: Subscriptions::default(),
            conn_labels: std::collections::HashMap::new(),
        }
    }
}
