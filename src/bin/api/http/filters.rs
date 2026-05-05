use std::sync::Arc;
use warp::Filter;

use super::super::email::EmailStore;
use super::super::sync::MemSync;

use fcore::{Env, Tag};

use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, IpAddrMask, MetricStorage,
    NodeStorageOperations, SubscriptionOperations,
};

/// Provides application state filter
pub fn with_sync<T, C, S>(
    mem_sync: MemSync<T, C, S>,
) -> impl Filter<Extract = (MemSync<T, C, S>,), Error = std::convert::Infallible> + Clone
where
    T: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static,
{
    warp::any().map(move || mem_sync.clone())
}

pub fn with_param_vec(
    param: Vec<u8>,
) -> impl Filter<Extract = (Vec<u8>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || param.clone())
}
pub fn with_param_envs(
    param: Vec<Env>,
) -> impl Filter<Extract = (Vec<Env>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || param.clone())
}
pub fn with_param_tags(
    param: Vec<Tag>,
) -> impl Filter<Extract = (Vec<Tag>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || param.clone())
}

pub fn with_param_ipaddrmask(
    param: IpAddrMask,
) -> impl Filter<Extract = (IpAddrMask,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || param.clone())
}

pub fn with_param_vec_string(
    param: Vec<String>,
) -> impl Filter<Extract = (Vec<String>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || param.clone())
}

pub fn with_metrics(
    metrics: Arc<MetricStorage>,
) -> impl Filter<Extract = (Arc<MetricStorage>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || metrics.clone())
}

pub fn with_email_store(
    email_store: EmailStore,
) -> impl Filter<Extract = (EmailStore,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || email_store.clone())
}
