//! Retire inactive installation peers once every eligible node was observed.

use chrono::{TimeDelta, Utc};
use std::{collections::HashMap, time::Duration};
use tokio::time::{self, MissedTickBehavior};
use uuid::Uuid;

use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, Node, NodeStorageOperations,
    Result, Subscription, SubscriptionOperations, Tag,
};

use super::{service::Service, sync::tasks::publish_conn_delete};

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
    pub async fn expire_installation_keys(
        &self,
        interval_sec: u64,
        timeout_sec: i64,
        freshness_sec: i64,
    ) {
        let Some(timeout) = TimeDelta::try_seconds(timeout_sec) else {
            tracing::error!("Installation inactivity timeout is out of range");
            return;
        };
        if interval_sec == 0 || timeout_sec <= 0 || freshness_sec <= 0 {
            tracing::error!("Installation inactivity intervals must be positive");
            return;
        }
        let mut interval = time::interval(Duration::from_secs(interval_sec));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let Some(cutoff) = Utc::now().checked_sub_signed(timeout) else {
                tracing::error!("Installation inactivity cutoff is out of range");
                return;
            };
            let candidates = match self
                .sync
                .db
                .conn()
                .list_idle_installation_candidates(cutoff)
                .await
            {
                Ok(candidates) => candidates,
                Err(error) => {
                    tracing::warn!(%error, "Failed to find inactive installations");
                    continue;
                }
            };
            for (sub_id, installation_id) in candidates {
                if let Err(error) = self
                    .expire_installation_if_idle(
                        &sub_id,
                        &installation_id,
                        timeout_sec,
                        freshness_sec,
                    )
                    .await
                {
                    tracing::warn!(%error, %installation_id, "Failed to retire inactive installation peers");
                }
            }
        }
    }

    async fn expire_installation_if_idle(
        &self,
        sub_id: &Uuid,
        installation_id: &Uuid,
        timeout_sec: i64,
        freshness_sec: i64,
    ) -> Result<()> {
        let _guard = self.sync.connection_write.lock().await;
        let db = self.sync.db.conn();
        let Some(installation) = db.installation_connections(sub_id, installation_id).await? else {
            return Ok(());
        };
        if installation.revoked {
            return Ok(());
        }

        let (expired, peers) = {
            // Registration and heartbeat updates mutate this cache. Keep the
            // eligible nodes stable until the DB commits the decision.
            let memory = self.sync.memory.read().await;
            let mut expected = Vec::new();
            let mut peers = HashMap::new();
            for (_, conn_id) in installation.connections {
                let Some(conn) = memory.connections.get(&conn_id) else {
                    return Ok(());
                };
                if conn.get_deleted() {
                    // The transaction checks that the selected peers still cover every live binding.
                    continue;
                }
                if conn.get_subscription_id().as_ref() != Some(sub_id)
                    || !memory.installation_conns.contains(&conn_id)
                {
                    return Ok(());
                }
                let tag = conn.get_proto().proto();
                if !matches!(tag, Tag::AmneziaWg | Tag::AmneziaWgMobile) {
                    return Ok(());
                }
                let Some(nodes) = memory.nodes.get_by_env(&conn.get_env()) else {
                    return Ok(());
                };
                let pin = memory.conn_nodes.get(&conn_id).copied();
                let Some(node_ids) = observable_nodes(&nodes, tag, pin) else {
                    return Ok(());
                };
                expected.extend(node_ids.into_iter().map(|node_id| (conn_id, node_id)));
                peers.insert(conn_id, conn.clone());
            }
            if expected.is_empty() {
                return Ok(());
            }
            expected.sort_unstable();
            expected.dedup();
            let expired = db
                .expire_idle_installation(
                    sub_id,
                    installation_id,
                    &expected,
                    Utc::now(),
                    timeout_sec,
                    freshness_sec,
                )
                .await?;
            (expired, peers)
        };
        if expired.is_empty() {
            return Ok(());
        }

        {
            let mut memory = self.sync.memory.write().await;
            for conn_id in &expired {
                if let Some(conn) = memory.connections.get_mut(conn_id) {
                    conn.set_deleted(true);
                    conn.set_modified_at();
                }
            }
        }
        // Keep node pins for the regular tombstone republisher, including when
        // this one-shot send fails. The DB transaction already stored the reason.
        for conn_id in &expired {
            if let Some(conn) = peers.get(conn_id) {
                if let Err(error) = publish_conn_delete(&self.sync, conn_id, conn).await {
                    tracing::warn!(%error, %conn_id, "Failed to publish inactive peer deletion");
                }
            }
        }
        tracing::info!(%installation_id, peers = expired.len(), "Retired inactive installation peers");
        Ok(())
    }
}

fn observable_nodes(nodes: &[Node], tag: Tag, pin: Option<Uuid>) -> Option<Vec<Uuid>> {
    let mut node_ids = Vec::new();
    for node in nodes
        .iter()
        .filter(|node| pin.is_none_or(|pin| node.uuid == pin))
    {
        if node.inbounds.is_empty() {
            // A node may still be serving while its runtime inbounds are unknown to the API.
            return None;
        }
        if node.inbounds.values().any(|inbound| inbound.tag == tag) {
            if !node.status.is_serving() {
                return None;
            }
            node_ids.push(node.uuid);
        }
    }
    (!node_ids.is_empty()).then_some(node_ids)
}
