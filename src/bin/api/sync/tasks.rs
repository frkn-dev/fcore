use chrono::{Duration, Utc};
use futures::future::join_all;
use tracing::{debug, error, info, warn};

use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, ConnectionStorageApiOperations,
    Env, Node, NodeStatus, NodeStorageOperations, Status, Subscription, SubscriptionOperations,
    SubscriptionStorageOperations, SyncError, Topic,
};

use super::super::{
    http::request::Subscription as SubReq,
    postgres::connection::ConnRow,
    subscription_audit,
};
use super::MemSync;

type SyncResult<T> = std::result::Result<T, SyncError>;

// Input validation traits
trait Validate {
    fn validate(&self) -> SyncResult<()>;
}

impl Validate for Node {
    fn validate(&self) -> SyncResult<()> {
        Ok(())
    }
}

impl Validate for Connection {
    fn validate(&self) -> SyncResult<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
pub trait SyncOp<N, C, S>
where
    N: NodeStorageOperations + Send + Sync + Clone + 'static,
    C: ConnectionBaseOperations
        + ConnectionApiOperations
        + Send
        + Sync
        + Clone
        + 'static
        + From<Connection>
        + std::cmp::PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + 'static,
{
    async fn add_node(&self, node_id: &uuid::Uuid, node: Node) -> SyncResult<Status>;
    async fn delete_node(&self, uuid: &uuid::Uuid) -> SyncResult<Status>;
    async fn add_conn(
        &self,
        conn_id: &uuid::Uuid,
        conn: Connection,
        label: Option<String>,
    ) -> SyncResult<Status>;
    async fn add_sub(&self, sub: Subscription) -> SyncResult<Status>;
    async fn delete_connection(&self, conn_id: &uuid::Uuid, conn: &C) -> SyncResult<Status>;
    async fn restore_connection(&self, conn_id: &uuid::Uuid) -> SyncResult<Status>;
    async fn update_node_status(
        &self,
        uuid: &uuid::Uuid,
        env: &Env,
        status: NodeStatus,
    ) -> SyncResult<()>;
    async fn update_sub(&self, sub_id: &uuid::Uuid, sub_req: SubReq) -> SyncResult<Status>;
    async fn add_days(&self, sub_id: &uuid::Uuid, days: i64) -> SyncResult<Status>;
    async fn add_days_inner(&self, sub_id: &uuid::Uuid, days: i64) -> SyncResult<Status>;
    async fn restore_connections_by_subscription(
        &self,
        sub_id: &uuid::Uuid,
    ) -> SyncResult<Vec<uuid::Uuid>>;
}

#[async_trait::async_trait]
impl<N, C, S> SyncOp<N, C, S> for MemSync<N, C, S>
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
    Connection: From<C>,
    S: SubscriptionOperations
        + Send
        + Sync
        + Clone
        + 'static
        + PartialEq
        + std::convert::From<Subscription>,
{
    async fn add_node(&self, node_id: &uuid::Uuid, node: Node) -> SyncResult<Status> {
        info!("Adding node: {}", node_id);

        // Validate input
        node.validate()?;

        // Insert into database first
        if let Err(e) = self.db.node().upsert(*node_id, node.clone()).await {
            error!("Failed to insert node {} into database: {}", node_id, e);
            return Err(SyncError::Database(e));
        }

        // Insert into memory
        let result = {
            let mut memory = self.memory.write().await;
            memory.nodes.add(node.clone())
        };

        match result {
            Ok(Status::Ok(_)) | Ok(Status::AlreadyExist(_)) => {
                info!("Successfully added node: {}", node_id);
                Ok(Status::Ok(*node_id))
            }
            Ok(Status::Updated(_)) => {
                info!("Successfully added node: {}", node_id);
                Ok(Status::Ok(*node_id))
            }
            Ok(Status::NotModified(id)) => {
                debug!("Node {} not modified", id);
                Ok(Status::NotModified(id))
            }
            Ok(other) => {
                error!(
                    "Unexpected operation status for node {}: {:?}",
                    node_id, other
                );
                Err(SyncError::Memory(format!(
                    "Unexpected operation status: {:?}",
                    other
                )))
            }
            Err(e) => {
                error!("Memory error adding node {}: {}", node_id, e);
                Err(SyncError::Memory(e.to_string()))
            }
        }
    }

    async fn delete_node(&self, uuid: &uuid::Uuid) -> SyncResult<Status> {
        info!("Deleting node: {}", uuid);

        if let Err(e) = self.db.node().delete(uuid).await {
            error!("Failed to delete node {} from database: {}", uuid, e);
            return Err(SyncError::Database(e));
        }

        let result = {
            let mut memory = self.memory.write().await;
            memory.nodes.remove(uuid)
        };

        match result {
            Ok(Some(_)) => {
                info!("Successfully deleted node: {}", uuid);
                Ok(Status::Ok(*uuid))
            }
            Ok(None) => {
                warn!("Node {} not found in memory", uuid);
                Ok(Status::NotFound(*uuid))
            }
            Err(e) => {
                error!("Memory error deleting node {}: {}", uuid, e);
                Err(SyncError::Memory(e.to_string()))
            }
        }
    }

    async fn add_sub(&self, sub: Subscription) -> SyncResult<Status> {
        info!("Adding subscription: {}", sub.id);

        subscription_audit::log_days_change(
            "created",
            sub.id,
            None,
            sub.expires_at(),
            None,
            "SyncOp::add_sub",
        );

        // Insert into database
        if let Err(e) = self.db.sub().create(&sub).await {
            error!(
                "Failed to insert subscription {} into database: {}",
                sub.id, e
            );
            return Err(SyncError::Database(e));
        }

        // Insert into memory
        let result = {
            let mut memory = self.memory.write().await;
            memory.subscriptions.add(sub.clone().into())
        };

        match result {
            Status::Ok(id) => {
                info!("Successfully added subscription: {}", id);
                Ok(Status::Ok(id))
            }
            Status::Updated(id) => {
                info!("Successfully updated subscription: {}", id);
                Ok(Status::Updated(id))
            }
            Status::AlreadyExist(id) => {
                info!("Subscription already exists: {}", id);
                Ok(Status::AlreadyExist(id))
            }
            Status::BadRequest(id, msg) => {
                warn!("Bad request for subscription {}: {}", id, msg);
                Ok(Status::BadRequest(id, msg))
            }
            other => {
                error!(
                    "Unexpected operation status for subscription {}: {:?}",
                    sub.id, other
                );
                Err(SyncError::Memory(format!(
                    "Unexpected operation status: {:?}",
                    other
                )))
            }
        }
    }

    async fn add_conn(
        &self,
        conn_id: &uuid::Uuid,
        conn: Connection,
        label: Option<String>,
    ) -> SyncResult<Status> {
        info!("Adding connection: {}", conn_id);

        // Validate input
        conn.validate()?;

        // Create database row. The label is PG-only (connections.label):
        // it is persisted here and mirrored into the api-side conn_labels
        // side map, but never becomes part of the rkyv payload to nodes.
        let mut conn_row: ConnRow = (*conn_id, conn.clone()).into();
        conn_row.label = label.clone();

        // Insert into database first
        if let Err(e) = self.db.conn().insert(conn_row).await {
            error!(
                "Failed to insert connection {} into database: {}",
                conn_id, e
            );
            return Err(SyncError::Database(e));
        }

        // Insert into memory
        let result = {
            let mut memory = self.memory.write().await;
            if let Some(label) = label {
                memory.conn_labels.insert(*conn_id, label);
            }
            ConnectionStorageApiOperations::add(
                &mut memory.connections,
                conn_id,
                conn.clone().into(),
            )
        };

        match result {
            Ok(Status::Ok(id)) | Ok(Status::AlreadyExist(id)) => {
                info!("Successfully added connection: {}", id);
                Ok(Status::Ok(id))
            }
            Ok(Status::BadRequest(id, msg)) => {
                warn!("Bad request for connection {}: {}", id, msg);
                Ok(Status::BadRequest(id, msg))
            }
            Ok(other) => {
                error!(
                    "Unexpected operation status for connection {}: {}",
                    conn_id, other
                );
                Err(SyncError::Memory(format!(
                    "Unexpected operation status: {}",
                    other
                )))
            }
            Err(e) => {
                error!("Memory error adding connection {}: {}", conn_id, e);
                Err(SyncError::Memory(e.to_string()))
            }
        }
    }

    async fn delete_connection(&self, conn_id: &uuid::Uuid, conn: &C) -> SyncResult<Status> {
        info!("Starting deletion process for connection: {}", conn_id);

        {
            let memory = self.memory.read().await;
            if let Some(existing) = memory.connections.get(conn_id) {
                if existing.get_deleted() {
                    info!("Connection {} already marked as deleted, skipping", conn_id);
                    return Ok(Status::NotModified(*conn_id));
                }
            } else {
                warn!("Connection {} not found in memory", conn_id);
                return Ok(Status::NotFound(*conn_id));
            }
        }

        if let Err(e) = self.db.conn().delete(conn_id).await {
            error!(
                "CRITICAL: Failed to delete connection {} from DB: {}",
                conn_id, e
            );
            return Err(SyncError::Database(e));
        }
        debug!("Connection {} successfully removed from database", conn_id);

        let msg = vec![conn.as_delete_message(conn_id)];
        let topic = if conn.get_token().is_some() {
            Topic::Auth
        } else {
            conn.get_env().into()
        };

        match rkyv::to_bytes::<_, 1024>(&msg) {
            Ok(bytes) => {
                info!("Publishing delete command to topic: {}", topic);
                if let Err(e) = self.publisher.send_binary(&topic, bytes.as_ref()).await {
                    error!(
                        "NETWORK ERROR: Failed to send delete signal for {} to bus: {:?}",
                        conn_id, e
                    );

                    return Err(SyncError::Zmq(e));
                }
            }
            Err(e) => {
                error!("SERIALIZATION ERROR for connection {}: {:?}", conn_id, e);
                return Err(SyncError::RkyvSerialize(e));
            }
        }

        {
            let mut memory = self.memory.write().await;
            if let Some(conn_mut) = memory.connections.get_mut(conn_id) {
                conn_mut.set_deleted(true);
                conn_mut.set_modified_at();
                info!(
                    "Memory state updated: connection {} marked as deleted",
                    conn_id
                );
            }
            // The label is per-device; a deleted device drops it from the
            // side map (PG keeps the column, so a full reload or a restore
            // followed by a periodic sync re-populates it).
            memory.conn_labels.remove(conn_id);
            // A deleted share child leaves the hidden set as well.
            memory.share_conns.remove(conn_id);
        }

        info!("Successfully completed deletion flow for: {}", conn_id);
        Ok(Status::Ok(*conn_id))
    }

    async fn restore_connections_by_subscription(
        &self,
        sub_id: &uuid::Uuid,
    ) -> SyncResult<Vec<uuid::Uuid>>
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
        Connection: From<C>,
    {
        let conns_to_restore: Vec<(uuid::Uuid, Connection)> = {
            let mem = self.memory.read().await;

            match mem.connections.get_by_subscription_id(sub_id) {
                Some(conns) => conns
                    .iter()
                    .filter(|(_, c)| c.get_deleted())
                    .map(|(id, c)| (*id, c.clone().into()))
                    .collect(),
                None => Vec::new(),
            }
        };

        if conns_to_restore.is_empty() {
            return Ok(vec![]);
        }

        let this = self.clone();

        let tasks = conns_to_restore.into_iter().map(|(conn_id, conn)| {
            let this = this.clone();
            async move {
                let msg = vec![conn.as_update_message(&conn_id)];

                let bytes = match rkyv::to_bytes::<_, 1024>(&msg) {
                    Ok(b) => b,
                    Err(e) => {
                        error!("Serialization failed for {}: {:?}", conn_id, e);
                        return None;
                    }
                };

                let topic = if conn.get_token().is_some() {
                    Topic::Auth
                } else {
                    conn.get_env().into()
                };

                if let Err(e) = this.publisher.send_binary(&topic, bytes.as_ref()).await {
                    error!(
                        "Failed to send restore message for {} to {}: {:?}",
                        conn_id, topic, e
                    );
                    return None;
                }

                match this.restore_connection(&conn_id).await {
                    Ok(Status::Ok(_)) | Ok(Status::Updated(_)) => {
                        debug!("Connection {} restored", conn_id);
                        Some(conn_id)
                    }
                    Ok(status) => {
                        warn!("Connection {} not restored: {:?}", conn_id, status);
                        None
                    }
                    Err(e) => {
                        error!("Failed to restore connection {}: {:?}", conn_id, e);
                        None
                    }
                }
            }
        });

        let results = join_all(tasks).await;
        Ok(results.into_iter().flatten().collect())
    }

    async fn restore_connection(&self, conn_id: &uuid::Uuid) -> SyncResult<Status> {
        info!("Restoring connection: {}", conn_id);

        // Check if connection exists and get its current state
        let current_conn = {
            let memory = self.memory.read().await;
            memory.connections.get(conn_id).cloned()
        };

        let _conn = match current_conn {
            Some(c) => c,
            None => {
                warn!("Connection {} not found for restoration", conn_id);
                return Ok(Status::NotFound(*conn_id));
            }
        };

        // Undelete from database first
        if let Err(e) = self.db.conn().restore(conn_id).await {
            error!(
                "Failed to restore connection {} from database: {}",
                conn_id, e
            );
            return Err(SyncError::Database(e));
        }

        // Mark as undeleted in memory
        {
            let mut memory = self.memory.write().await;
            if let Some(conn_mut) = memory.connections.get_mut(conn_id) {
                conn_mut.set_deleted(false);
                conn_mut.set_modified_at();
            }
        }

        info!("Successfully restored connection: {}", conn_id);
        Ok(Status::Ok(*conn_id))
    }

    async fn update_node_status(
        &self,
        uuid: &uuid::Uuid,
        env: &Env,
        status: NodeStatus,
    ) -> SyncResult<()> {
        debug!(
            "Updating node status: {} in env {} to {:?}",
            uuid, env, status
        );

        // -------------------------
        // 1. DB update FIRST (source of truth)
        // -------------------------
        if let Err(e) = self.db.node().update_status(uuid, status).await {
            error!("Failed DB update for node {}: {}", uuid, e);
            return Err(SyncError::Database(e));
        }

        // -------------------------
        // 2. Memory update AFTER DB
        // -------------------------
        {
            let memory = self.memory.write().await;

            // IMPORTANT: DO NOT rely on env here
            if let Some(mut node) = memory.nodes.get_by_id(uuid) {
                node.status = status;
            } else {
                debug!("Node {} not found in memory (will be synced later)", uuid);
            }
        }

        debug!("Successfully updated node {} status to {:?}", uuid, status);

        Ok(())
    }

    async fn update_sub(&self, sub_id: &uuid::Uuid, req: SubReq) -> SyncResult<Status> {
        info!("Updating subscription: {}", sub_id);

        let old_expires_at = {
            let mem = self.memory.read().await;
            mem.subscriptions.get(sub_id).and_then(|s| s.expires_at())
        };

        {
            let mut memory = self.memory.write().await;

            let sub = match memory.subscriptions.find_by_id_mut(sub_id) {
                Some(s) => s,
                None => {
                    warn!("Subscription {} not found for update", sub_id);
                    return Ok(Status::NotFound(*sub_id));
                }
            };

            if let Some(days) = req.days {
                sub.extend(days);
            }

            if let Some(ref_code) = req.refer_code.clone() {
                sub.set_refer_code(ref_code);
            }

            if let Some(limit_bytes) = req.limit_bytes {
                sub.set_limit_bytes(limit_bytes);
            }

            let expires_at = sub
                .expires_at()
                .ok_or_else(|| SyncError::InconsistentState {
                    resource: "Subscription".to_string(),
                    id: *sub_id,
                })?;

            subscription_audit::log_days_change(
                "updated",
                *sub_id,
                old_expires_at,
                Some(expires_at),
                req.days,
                "SyncOp::update_sub",
            );

            if let Err(e) = self
                .db
                .sub()
                .update_subscription(
                    *sub_id,
                    expires_at,
                    &sub.refer_code(),
                    sub.parent_id(),
                    sub.scope_env(),
                    sub.premium_token(),
                )
                .await
            {
                error!(
                    "Failed to update subscription {} in database: {}",
                    sub_id, e
                );
                return Err(SyncError::Database(e));
            }
        }

        if let Some(days) = req.days {
            let _ = days;
        }

        info!("Successfully updated subscription: {}", sub_id);
        Ok(Status::Updated(*sub_id))
    }

    async fn add_days(&self, sub_id: &uuid::Uuid, days: i64) -> SyncResult<Status> {
        let status = self.add_days_inner(sub_id, days).await?;
        Ok(status)
    }

    async fn add_days_inner(&self, sub_id: &uuid::Uuid, days: i64) -> SyncResult<Status> {
        let sub_db = self.db.sub();

        let (was_inactive, old_expires_at) = {
            let mem = self.memory.read().await;
            let sub = mem.subscriptions.get(sub_id);
            (
                sub.map(|s| !s.is_active()).unwrap_or(false),
                sub.and_then(|s| s.expires_at()),
            )
        };

        let new_expires_at = {
            let mut mem = self.memory.write().await;

            let sub = match mem.subscriptions.find_by_id_mut(sub_id) {
                Some(s) => s,
                None => {
                    warn!("Subscription {} not found for update", sub_id);
                    return Ok(Status::NotFound(*sub_id));
                }
            };

            let now = Utc::now();

            let new_expires = match sub.expires_at() {
                Some(exp) if exp > now => exp + Duration::days(days),
                _ => now + Duration::days(days),
            };
            let _ = sub.set_expires_at(new_expires);
            Some(new_expires)
        };

        subscription_audit::log_days_change(
            "days_added",
            *sub_id,
            old_expires_at,
            new_expires_at,
            Some(days),
            "SyncOp::add_days_inner",
        );

        match sub_db.add_days(sub_id, days).await {
            Ok(sub) => {
                if was_inactive {
                    info!(
                        "Restoring connections after subscription activation {}",
                        sub_id
                    );

                    match self.restore_connections_by_subscription(sub_id).await {
                        Ok(restored) => {
                            debug!(
                                "Post-update restore: {} connections restored for {}",
                                restored.len(),
                                sub_id
                            );
                        }
                        Err(e) => {
                            error!("Post-update restore FAILED for {}: {:?}", sub_id, e);
                        }
                    }
                }
                Ok(Status::Updated(sub.id))
            }
            Err(e) => Err(SyncError::Database(e)),
        }
    }

}
