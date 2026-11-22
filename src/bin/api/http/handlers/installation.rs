use serde::Serialize;
use uuid::Uuid;
use warp::{http::StatusCode, reply::Reply};

use fcore::{
    http::{helpers as http, ResponseMessage},
    Connection, ConnectionApiOperations, ConnectionBaseOperations, NodeStorageOperations,
    Subscription, SubscriptionOperations, SubscriptionStorageOperations,
};

use crate::sync::{
    tasks::{SyncOp, DELETE_REASON_DEVICE_KICK},
    MemSync,
};

use super::private::{check_user_owner, PrivateNodesQuery};

#[derive(Serialize)]
struct InstallationListItem {
    installation_uuid: Uuid,
    revoked: bool,
}

#[derive(Serialize)]
struct InstallationRevokeResponse {
    installation_uuid: Uuid,
    revoked: bool,
    connections_deleted: usize,
}

async fn check_user_installation_owner<N, C, S>(
    memory: &MemSync<N, C, S>,
    child_id: Uuid,
    owner_id: Uuid,
) -> Result<(), warp::reply::Response>
where
    N: NodeStorageOperations + Send + Sync + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Send
        + Sync
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    check_user_owner(memory, owner_id).await?;
    let mem = memory.memory.read().await;
    let allowed = mem
        .subscriptions
        .find_by_id(&child_id)
        .is_some_and(|child| {
            !child.is_deleted() && child.is_selfhosted_user() && child.parent_id() == Some(owner_id)
        });
    if !allowed {
        return Err(http::not_found("user not found").into_response());
    }
    Ok(())
}

pub async fn list_private_user_installations<N, C, S>(
    child_id: Uuid,
    query: PrivateNodesQuery,
    memory: MemSync<N, C, S>,
) -> Result<warp::reply::Response, warp::Rejection>
where
    N: NodeStorageOperations + Send + Sync + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Send
        + Sync
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let _guard = memory.connection_write.lock().await;
    if let Err(reply) =
        check_user_installation_owner(&memory, child_id, query.subscription_id).await
    {
        return Ok(reply);
    }

    let rows = match memory.db.conn().list_installations(&child_id).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(%error, "Failed to list user installations");
            return Ok(http::internal_error("installation list failed").into_response());
        }
    };
    let installations: Vec<_> = rows
        .into_iter()
        .map(|(installation_uuid, revoked)| InstallationListItem {
            installation_uuid,
            revoked,
        })
        .collect();

    Ok(warp::reply::json(&ResponseMessage {
        status: StatusCode::OK.as_u16(),
        message: "Ok".to_string(),
        response: Some(installations),
    })
    .into_response())
}

pub async fn revoke_private_user_installation<N, C, S>(
    child_id: Uuid,
    installation_id: Uuid,
    query: PrivateNodesQuery,
    memory: MemSync<N, C, S>,
) -> Result<warp::reply::Response, warp::Rejection>
where
    N: NodeStorageOperations + Send + Sync + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Send
        + Sync
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    // A disconnected request must not interrupt the peer removals after the
    // installation tombstone is written.
    tokio::spawn(async move {
        let _guard = memory.connection_write.lock().await;
        if let Err(reply) =
            check_user_installation_owner(&memory, child_id, query.subscription_id).await
        {
            return Ok(reply);
        }

        let connections = memory.db.conn();
        match connections
            .installation_connections(&child_id, &installation_id)
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => return Ok(http::not_found("installation not found").into_response()),
            Err(error) => {
                tracing::error!(%error, "Failed to look up user installation");
                return Ok(http::internal_error("installation revoke failed").into_response());
            }
        }

        let ids = match connections
            .revoke_installation(&child_id, &installation_id)
            .await
        {
            Ok(ids) => ids,
            Err(error) => {
                tracing::error!(%error, "Failed to mark user installation revoked");
                return Ok(http::internal_error("installation revoke failed").into_response());
            }
        };
        let connections_deleted = ids.len();
        let mut failed = false;
        for id in ids {
            let conn = memory.memory.read().await.connections.get(&id).cloned();
            let Some(conn) = conn else {
                // A row not yet visible in the cache must still be excluded from
                // subsequent syncs. The persistent installation tombstone denies
                // config refresh while the cache catches up.
                if let Err(error) = connections
                    .delete(&id, Some(DELETE_REASON_DEVICE_KICK))
                    .await
                {
                    tracing::error!(%error, connection_id = %id, "Failed to revoke uncached connection");
                    failed = true;
                }
                continue;
            };
            if let Err(error) =
                SyncOp::delete_connection(&memory, &id, &conn, Some(DELETE_REASON_DEVICE_KICK)).await
            {
                tracing::error!(%error, connection_id = %id, "Failed to revoke installation connection");
                failed = true;
            }
        }
        if failed {
            // A repeated DELETE retries any connection that failed its delete
            // sequence. The installation itself is already blocked in the DB.
            return Ok(http::internal_error("installation revoke failed").into_response());
        }

        Ok(warp::reply::json(&ResponseMessage {
            status: StatusCode::OK.as_u16(),
            message: "Ok".to_string(),
            response: Some(InstallationRevokeResponse {
                installation_uuid: installation_id,
                revoked: true,
                connections_deleted,
            }),
        })
        .into_response())
    })
    .await
    .unwrap_or_else(|error| {
        tracing::error!(%error, "Installation revoke task failed");
        Ok(http::internal_error("installation revoke failed").into_response())
    })
}
