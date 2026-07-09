use tracing::{error, Instrument};

use fcore::{
    http::{helpers as http, response::Instance},
    Connection, ConnectionApiOperations, ConnectionBaseOperations, Distributor, Error, Key,
    NodeStorageOperations, Status, Subscription, SubscriptionOperations,
};

use super::super::{
    super::subscription_audit,
    super::sync::{tasks::SyncOp, MemSync},
    param::KeyQueryParams,
    request::{ActivateKeyReq, KeyReq},
};

/// Get specific & validate key handler
pub async fn get_key_validate_handler<N, C, S>(
    params: KeyQueryParams,
    memory: MemSync<N, C, S>,
    secret: Vec<u8>,
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
    let code = params.key;
    let db = memory.db.key();

    if !code.is_valid(&secret) {
        return Ok(http::bad_request("Key is not valid"));
    }

    match db.get(code.as_str()).await {
        Some(key) => {
            if key.activated {
                return Ok(http::success_response(
                    "Key is valid and already activated".to_string(),
                    Some(key.id),
                    Instance::Key(key.clone()),
                ));
            }

            let instance = Instance::Key(key.clone());
            Ok(http::success_response(
                "Key is valid".to_string(),
                Some(key.id),
                instance,
            ))
        }
        None => Ok(http::not_found("Key is not found")),
    }
}

/// Post key handler
pub async fn post_key_handler<N, C, S>(
    req: KeyReq,
    memory: MemSync<N, C, S>,
    secret: Vec<u8>,
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
    const DEFAULT_DISTRIBUTOR: &str = "FRKN";
    let distributor_str = req.distributor.as_deref().unwrap_or(DEFAULT_DISTRIBUTOR);

    let days = req.days;
    let distributor = Distributor::new(distributor_str)
        .map_err(|_| Error::Custom("invalid distributor".to_string()))?;

    let db = memory.db.key();
    let key = Key::new(days, &distributor, &secret);

    match db.insert(&key).await {
        Ok(_) => {
            let msg = format!("Key {} is created", key.id);
            Ok(http::success_response(
                msg,
                Some(key.id),
                Instance::Key(key),
            ))
        }
        Err(e) => {
            error!("Failed to insert key: {:?}", e);
            Ok(http::bad_request("Key create error"))
        }
    }
}

/// Post activate key
pub async fn post_activate_key_handler<N, C, S>(
    req: ActivateKeyReq,
    trace_id_header: Option<String>,
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + From<Subscription> + PartialEq,
    Connection: From<C>,
{
    let trace_id = subscription_audit::trace_id_from_header(trace_id_header);
    let key_db = memory.db.key();

    let mut key = match key_db.get(&req.code).await {
        Some(k) => k,
        None => return Ok(http::not_found("Key not found")),
    };

    if key.activated {
        return Ok(http::bad_request("Key already activated"));
    }

    subscription_audit::log_transaction_start(req.subscription_id, Some(key.days as i64));

    match SyncOp::add_days(
        &memory,
        &req.subscription_id,
        key.days as i64,
    )
    .instrument(subscription_audit::transaction_span(
        "key_activate_handler",
        req.subscription_id,
        Some(trace_id),
    ))
    .await
    {
        Ok(Status::Updated(_)) => {
            key.activate(&req.subscription_id);
            if let Err(err) = key_db.activate(&key).await {
                return Ok(http::bad_request(&format!(
                    "Key activation failed: {}",
                    err
                )));
            }

            Ok(http::success_response(
                format!("Key {} activated", key.id),
                Some(key.id),
                Instance::Key(key),
            ))
        }
        Ok(Status::NotFound(_)) => Ok(http::not_found("Subscription not found")),
        Err(err) => Ok(http::bad_request(&format!("Failed to add days: {}", err))),
        _ => Ok(http::not_modified("")),
    }
}
