use std::sync::Arc;
use warp::Filter;

use super::super::sync::MemSync;

use fcore::http::AuthError;
use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, IpAddrMask, MetricStorage,
    NodeStorageOperations, SubscriptionOperations, SubscriptionStorageOperations,
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

pub fn with_param_ipaddrmask(
    param: IpAddrMask,
) -> impl Filter<Extract = (IpAddrMask,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || param.clone())
}

pub fn with_metrics(
    metrics: Arc<MetricStorage>,
) -> impl Filter<Extract = (Arc<MetricStorage>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || metrics.clone())
}

/// Authentication using either a service token, an admin token, or a
/// durable per-node token issued at private register (`node_…`).
pub fn with_service_or_admin_auth(
    service_token: Arc<String>,
    admin_token: String,
) -> impl Filter<Extract = (), Error = warp::Rejection> + Clone {
    warp::header::<String>("authorization")
        .and_then(move |auth_header: String| {
            let service_token = service_token.clone();
            let admin_token = admin_token.clone();
            async move {
                let token = auth_header.strip_prefix("Bearer ").unwrap_or("");
                if token == service_token.as_str()
                    || (!admin_token.is_empty() && token == admin_token)
                {
                    Ok(())
                } else {
                    Err(warp::reject::custom(AuthError("Unauthorized".to_string())))
                }
            }
        })
        .untuple_one()
}

/// Кто зовёт `/connections/sync`: наш сервис/админ-токен (env и топик берутся
/// из тела запроса) или `node_`-токен self-hosted ноды (только её собственный
/// env и только её init-топик). Отдельный фильтр, потому что остальные
/// management-маршруты `node_` по-прежнему не принимают.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionsSyncAuth {
    /// Наш сервис или админ: любая нода, любой env.
    Service,
    /// Токен ноды: uuid, к которому он привязан.
    Node(uuid::Uuid),
}

pub fn with_connections_sync_auth<T, C, S>(
    service_token: Arc<String>,
    admin_token: String,
    mem_sync: MemSync<T, C, S>,
) -> impl Filter<Extract = (ConnectionsSyncAuth,), Error = warp::Rejection> + Clone
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
    warp::header::<String>("authorization")
        .and(with_sync(mem_sync))
        .and_then(move |auth_header: String, mem_sync: MemSync<T, C, S>| {
            let service_token = service_token.clone();
            let admin_token = admin_token.clone();
            async move {
                let token = auth_header.strip_prefix("Bearer ").unwrap_or("");
                if token == service_token.as_str()
                    || (!admin_token.is_empty() && token == admin_token)
                {
                    return Ok(ConnectionsSyncAuth::Service);
                }
                // `node_`-токен привязан к ноде записью в node_access_tokens:
                // без этой сверки клиент мог бы прислать чужой uuid в теле
                // (topic) и вытянуть пиров dev/ru.
                if token.starts_with("node_") {
                    return match mem_sync.db.node_access_token().find_by_token(token).await {
                        Ok(Some(id)) => Ok(ConnectionsSyncAuth::Node(id)),
                        Ok(None) => Err(warp::reject::custom(AuthError(
                            "unknown node token".to_string(),
                        ))),
                        Err(e) => {
                            tracing::error!("node token lookup failed: {}", e);
                            Err(warp::reject::custom(AuthError("Unauthorized".to_string())))
                        }
                    };
                }
                Err(warp::reject::custom(AuthError("Unauthorized".to_string())))
            }
        })
}

pub fn with_mgmt_auth<T, C, S>(
    service_token: Arc<String>,
    admin_token: String,
    mem_sync: MemSync<T, C, S>,
) -> impl Filter<Extract = (), Error = warp::Rejection> + Clone
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
    warp::header::<String>("authorization")
        .and(with_sync(mem_sync))
        .and_then(move |auth_header: String, mem_sync: MemSync<T, C, S>| {
            let service_token = service_token.clone();
            let admin_token = admin_token.clone();
            async move {
                let token = auth_header.strip_prefix("Bearer ").unwrap_or("");
                if token == service_token.as_str()
                    || (!admin_token.is_empty() && token == admin_token)
                {
                    return Ok(());
                }
                // `node_`-токен живёт на машине self-hosted пользователя. Это
                // не management-доступ: такие ноды регистрируются только через
                // POST /v1/private/nodes (inst_ -> node_), где env выводится из
                // подписки. Иначе токен открывал регистрацию в общих dev/ru/wl
                // и отдавал чужих пиров.
                if token.starts_with("node_") {
                    tracing::warn!("node_token_used_as_mgmt: отказ на management-маршруте");
                    return Err(warp::reject::custom(AuthError(
                        "node token is not a management token".to_string(),
                    )));
                }
                Err(warp::reject::custom(AuthError("Unauthorized".to_string())))
            }
        })
        .untuple_one()
}

/// Premium user authentication via Bearer token (premium_token).
pub fn with_premium_auth<T, C, S>(
    mem_sync: MemSync<T, C, S>,
) -> impl Filter<Extract = (S,), Error = warp::Rejection> + Clone
where
    T: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq,
{
    warp::header::optional::<String>("authorization")
        .and(with_sync(mem_sync))
        .and_then(
            |auth: Option<String>, mem_sync: MemSync<T, C, S>| async move {
                let token = auth
                    .and_then(|h| h.strip_prefix("Bearer ").map(|s| s.to_string()))
                    .ok_or_else(|| warp::reject::custom(AuthError("Unauthorized".to_string())))?;

                let mem = mem_sync.memory.read().await;
                let sub = mem
                    .subscriptions
                    .find_by_premium_token(&token)
                    .cloned()
                    .ok_or_else(|| warp::reject::custom(AuthError("Unauthorized".to_string())))?;

                Ok::<_, warp::Rejection>(sub)
            },
        )
}
