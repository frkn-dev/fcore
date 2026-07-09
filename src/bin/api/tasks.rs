use chrono::Utc;
use rand::Rng;
use std::collections::HashMap;
use std::time::Duration;

use tracing::{debug, error, info, warn};

use fcore::{
    measure_time, Connection, ConnectionApiOperations, ConnectionBaseOperations,
    ConnectionStorageApiOperations, Env, NodeStatus, NodeStorageOperations, Result, Status,
    Subscription, SubscriptionOperations, SubscriptionStorageOperations,
};

use super::{
    postgres::{connection::ConnWatermark, pg::Tasks as MemoryCacheTasks},
    service::{Cache, Service},
    sync::tasks::SyncOp,
    traffic,
};

#[async_trait::async_trait]
pub trait Tasks {
    async fn get_state_from_db(&self) -> Result<()>;
    async fn periodic_db_sync(&self, interval_sec: u64);
    async fn cleanup_expired_connections(&self, interval_sec: u64);
    async fn cleanup_expired_subscriptions(&self, interval_sec: u64);
    async fn restore_subscriptions(&self, interval_sec: u64);
    async fn monitor_node_heartbeats(&self, check_interval_sec: u64, offline_threshold_sec: u64);
    async fn persist_connection_traffic(&self, interval_sec: u64);
}

#[async_trait::async_trait]
impl<N, C, S> Tasks for Service<N, C, S>
where
    N: NodeStorageOperations + Send + Sync + Clone + 'static + std::default::Default,
    C: ConnectionBaseOperations
        + ConnectionApiOperations
        + Send
        + Sync
        + Clone
        + 'static
        + PartialEq
        + From<Connection>
        + Into<Connection>,
    S: SubscriptionOperations
        + Send
        + Sync
        + Clone
        + 'static
        + PartialEq
        + From<Subscription>
        + std::default::Default,
    Connection: From<C>,
    Vec<(uuid::Uuid, Connection)>: FromIterator<(uuid::Uuid, C)>,
{
    async fn cleanup_expired_connections(&self, interval_sec: u64) {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_sec));

        loop {
            interval.tick().await;

            debug!("Run cleanup conections task");

            let now = Utc::now();
            let expired_conns: Vec<(uuid::Uuid, Connection)> = {
                let memory = self.sync.memory.read().await;
                memory
                    .connections
                    .iter()
                    .filter_map(|(id, conn)| {
                        // Connections that belong to a subscription are managed by
                        // cleanup_expired_subscriptions / restore_subscriptions via the
                        // sync bus. Only standalone connections with their own expires_at
                        // should be cleaned up here.
                        if conn.get_subscription_id().is_some() {
                            return None;
                        }

                        if let Some(expires_at) = conn.get_expires_at() {
                            if expires_at <= now && !conn.get_deleted() {
                                Some((*id, conn.clone()))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect()
            };

            for (conn_id, conn) in expired_conns {
                match SyncOp::delete_connection(&self.sync, &conn_id, &conn.into()).await {
                    Ok(Status::Ok(_)) => {
                        info!("Expired connection {} deleted", conn_id);
                    }
                    Ok(status) => {
                        warn!("Connection {} could not be deleted: {:?}", conn_id, status);
                    }
                    Err(e) => {
                        error!("Failed to delete expired connection {}: {:?}", conn_id, e);
                    }
                }
            }
        }
    }

    async fn cleanup_expired_subscriptions(&self, interval_sec: u64) {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_sec));

        loop {
            interval.tick().await;
            debug!("Run cleanup subscriptions task");

            let expired_subs: Vec<uuid::Uuid> = {
                let mem = self.sync.memory.read().await;
                mem.subscriptions
                    .iter()
                    .filter_map(|(id, sub)| if !sub.is_active() { Some(*id) } else { None })
                    .collect()
            };

            for sub_id in expired_subs {
                let conns_to_delete: Vec<(uuid::Uuid, Connection)> = {
                    let mem = self.sync.memory.read().await;
                    mem.connections
                        .get_by_subscription_id(&sub_id)
                        .map(|conns| {
                            conns
                                .iter()
                                .filter(|(_id, c)| !c.get_deleted())
                                .map(|(id, c)| (*id, c.clone()))
                                .collect()
                        })
                        .unwrap_or_default()
                };

                for (conn_id, conn) in conns_to_delete {
                    match SyncOp::delete_connection(&self.sync, &conn_id, &conn.into()).await {
                        Ok(Status::Ok(_)) => {
                            info!("Expired connection {} deleted", conn_id);
                        }
                        Ok(status) => {
                            warn!(
                                "!!! Connection {} could not be deleted: {:?}",
                                conn_id, status
                            );
                        }
                        Err(e) => {
                            error!("Failed to delete expired connection {}: {:?}", conn_id, e);
                        }
                    }
                }
            }
        }
    }

    async fn restore_subscriptions(&self, interval_sec: u64) {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_sec));

        loop {
            interval.tick().await;
            debug!("Run restore subscriptions task");

            let active_subs: Vec<uuid::Uuid> = {
                let mem = self.sync.memory.read().await;
                mem.subscriptions
                    .iter()
                    .filter_map(|(id, sub)| if sub.is_active() { Some(*id) } else { None })
                    .collect()
            };

            for sub_id in active_subs {
                match SyncOp::restore_connections_by_subscription(&self.sync, &sub_id).await {
                    Ok(restored) => {
                        if !restored.is_empty() {
                            info!(
                                "Restored {} connections for subscription {}",
                                restored.len(),
                                sub_id
                            );
                        }
                    }
                    Err(e) => {
                        error!("Failed to restore expired connection {}: {:?}", sub_id, e);
                    }
                }
            }
        }
    }

    async fn persist_connection_traffic(&self, interval_sec: u64) {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_sec));

        loop {
            interval.tick().await;
            let now = Utc::now();

            let watermarks = match self.sync.db.conn().all_watermarks().await {
                Ok(w) => w,
                Err(e) => {
                    error!("Failed to load connection traffic watermarks: {}", e);
                    continue;
                }
            };
            let watermark_map: HashMap<uuid::Uuid, ConnWatermark> =
                watermarks.into_iter().map(|w| (w.conn_id, w)).collect();

            let conns: Vec<(uuid::Uuid, Connection)> = {
                let memory = self.sync.memory.read().await;
                memory
                    .connections
                    .iter()
                    .filter_map(|(id, conn)| {
                        if conn.get_deleted() || conn.get_subscription_id().is_none() {
                            None
                        } else {
                            Some((*id, conn.clone().into()))
                        }
                    })
                    .collect()
            };

            if conns.is_empty() {
                continue;
            }

            debug!("Persisting traffic for {} connections", conns.len());
            let mut persisted = 0usize;

            for (conn_id, conn) in conns {
                let sub_id = conn.get_subscription_id().unwrap();
                let env = conn.get_env().to_string();

                let created_at = {
                    let memory = self.sync.memory.read().await;
                    memory
                        .subscriptions
                        .find_by_id(&sub_id)
                        .map(|s| s.created_at())
                };
                let Some(created_at) = created_at else {
                    continue;
                };

                let wm = watermark_map
                    .get(&conn_id)
                    .cloned()
                    .unwrap_or(ConnWatermark {
                        conn_id,
                        subscription_id: sub_id,
                        env: env.clone(),
                        uplink: 0,
                        downlink: 0,
                        last_persist_at: now,
                    });

                let segments = traffic::connection_deltas_between(
                    &self.metrics,
                    &conn_id,
                    created_at,
                    wm.last_persist_at,
                    now,
                );

                let mut total_up: u64 = 0;
                let mut total_down: u64 = 0;
                let mut failed = false;

                for seg in &segments {
                    if let Err(e) = self
                        .sync
                        .db
                        .traffic()
                        .upsert_bucket(
                            conn_id,
                            sub_id,
                            &env,
                            "day",
                            seg.day_bucket,
                            seg.uplink as i64,
                            seg.downlink as i64,
                        )
                        .await
                    {
                        error!(
                            "Failed to persist daily traffic for connection {}: {}",
                            conn_id, e
                        );
                        failed = true;
                        break;
                    }

                    if let Err(e) = self
                        .sync
                        .db
                        .traffic()
                        .upsert_bucket(
                            conn_id,
                            sub_id,
                            &env,
                            "month",
                            seg.month_bucket,
                            seg.uplink as i64,
                            seg.downlink as i64,
                        )
                        .await
                    {
                        error!(
                            "Failed to persist monthly traffic for connection {}: {}",
                            conn_id, e
                        );
                        failed = true;
                        break;
                    }

                    total_up += seg.uplink;
                    total_down += seg.downlink;
                }

                if failed {
                    continue;
                }

                let new_uplink = wm.uplink.saturating_add(total_up as i64);
                let new_downlink = wm.downlink.saturating_add(total_down as i64);

                if let Err(e) = self
                    .sync
                    .db
                    .conn()
                    .update_watermark(conn_id, new_uplink, new_downlink, now)
                    .await
                {
                    error!(
                        "Failed to update traffic watermark for connection {}: {}",
                        conn_id, e
                    );
                    continue;
                }

                if !segments.is_empty() {
                    persisted += 1;
                }
            }

            info!("Persisted traffic for {} connections", persisted);
        }
    }

    async fn periodic_db_sync(&self, interval_sec: u64) {
        let base_interval = Duration::from_secs(interval_sec);

        loop {
            let jitter = rand::thread_rng().gen_range(0..=30);

            tokio::time::sleep(base_interval + Duration::from_secs(jitter)).await;

            match measure_time(self.get_state_from_db(), "Periodic DB Sync").await {
                Ok(_) => {
                    info!("Periodic DB sync completed successfully");
                }
                Err(err) => {
                    error!("Periodic DB sync failed: {:?}", err);
                }
            }
        }
    }

    async fn get_state_from_db(&self) -> Result<()> {
        let db = self.sync.db.clone();

        let node_repo = db.node();
        let conn_repo = db.conn();
        let sub_repo = db.sub();

        let (nodes, conns, subscriptions) =
            tokio::try_join!(node_repo.all(), conn_repo.all(), sub_repo.all())?;

        let mut tmp_mem: Cache<N, C, S> = Cache::new();

        for node in nodes {
            tmp_mem.add_node(node).await?;
        }

        for conn in conns {
            tmp_mem.add_conn(conn).await?;
        }

        for sub in subscriptions {
            tmp_mem.add_subscription(sub).await;
        }

        let mut mem = self.sync.memory.write().await;
        *mem = tmp_mem;

        Ok(())
    }

    async fn monitor_node_heartbeats(&self, check_interval_sec: u64, offline_threshold_sec: u64) {
        let mut interval = tokio::time::interval(Duration::from_secs(check_interval_sec));
        let threshold_ms = (offline_threshold_sec * 1000) as i64;

        loop {
            interval.tick().await;
            tracing::debug!("Running node heartbeat monitor task");

            let now_ms = chrono::Utc::now().timestamp_millis();

            let nodes_snapshot: Vec<(uuid::Uuid, Env, NodeStatus)> = {
                let memory = self.sync.memory.read().await;
                memory
                    .nodes
                    .iter_nodes()
                    .map(|(id, node)| (*id, node.env.clone(), node.status.clone()))
                    .collect()
            };

            for (node_id, env, current_status) in nodes_snapshot {
                let last_heartbeat_opt = self.metrics.get_last_heartbeat(&node_id);
                let is_offline = match last_heartbeat_opt {
                    Some(ts) => (now_ms - ts) > threshold_ms,
                    None => true,
                };

                if is_offline && current_status == NodeStatus::Offline {
                    continue;
                }
                if !is_offline && current_status != NodeStatus::Offline {
                    continue;
                }

                let new_status = if is_offline {
                    NodeStatus::Offline
                } else {
                    NodeStatus::Online
                };

                {
                    let mut memory = self.sync.memory.write().await;
                    if let Some(node) = memory.nodes.get_mut(&env, &node_id) {
                        node.status = new_status.clone();
                    }
                }

                if let Err(e) =
                    SyncOp::update_node_status(&self.sync, &node_id, &env, new_status).await
                {
                    tracing::error!("Failed to update node {} status: {:?}", node_id, e);
                } else {
                    tracing::info!("Node {} status changed to {:?}", node_id, new_status);
                }
            }
        }
    }
}
