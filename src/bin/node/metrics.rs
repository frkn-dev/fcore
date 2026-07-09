use super::node::Node;

#[cfg(feature = "xray")]
use fcore::{Prefix, StatsOp, Tag};

use fcore::{ConnectionBaseOperations, HasMetrics, MetricBuffer, Node as MemNode};

impl<C> HasMetrics for Node<C>
where
    C: ConnectionBaseOperations + Send + Sync + Clone + 'static,
{
    fn metrics(&self) -> &MetricBuffer {
        &self.metrics
    }

    fn node_settings(&self) -> &MemNode {
        &self.node
    }
}

#[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
#[async_trait::async_trait]
pub trait BusinessMetrics {
    #[cfg(feature = "xray")]
    async fn collect_inbound_metrics(&self);
    #[cfg(feature = "xray")]
    async fn collect_user_metrics(&self);
    #[cfg(feature = "wireguard")]
    async fn collect_wg_metrics(&self);
    #[cfg(feature = "amnezia-wg")]
    async fn collect_awg_metrics(&self);
}

#[cfg(any(feature = "wireguard", feature = "amnezia-wg"))]
/// Considers a peer online if the last handshake was no more than 3 minutes ago.
fn peer_online_by_handshake(last_handshake_ms: Option<u64>) -> f64 {
    const ONLINE_TIMEOUT_MS: i64 = 3 * 60 * 1000;
    let Some(hs_ms) = last_handshake_ms else { return 0.0 };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let hs_ms = hs_ms as i64;
    if now_ms >= hs_ms && (now_ms - hs_ms) <= ONLINE_TIMEOUT_MS {
        1.0
    } else {
        0.0
    }
}

#[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
#[async_trait::async_trait]
impl<C> BusinessMetrics for Node<C>
where
    C: ConnectionBaseOperations + Send + Sync + Clone + 'static,
{
    #[cfg(feature = "xray")]
    async fn collect_inbound_metrics(&self) {
        let node_uuid = self.node.uuid;
        let base_tags = self.node.get_base_tags();

        for tag in self.node.inbounds.keys() {
            if matches!(tag, Tag::Hysteria2 | Tag::Mtproto) {
                continue;
            }

            let prefix = Prefix::InboundPrefix(*tag);

            if let Ok(stats) = self.inbound(prefix).await {
                let mut metric_tags = base_tags.clone();
                metric_tags.insert("inbound_tag".to_string(), tag.to_string());

                let metric_prefix = format!("net.inbound.{}", tag);

                self.metrics.push(
                    node_uuid,
                    &format!("{}.downlink", metric_prefix),
                    stats.downlink as f64,
                    metric_tags.clone(),
                );
                self.metrics.push(
                    node_uuid,
                    &format!("{}.uplink", metric_prefix),
                    stats.uplink as f64,
                    metric_tags.clone(),
                );
                self.metrics.push(
                    node_uuid,
                    &format!("{}.connections", metric_prefix),
                    stats.conn_count as f64,
                    metric_tags,
                );
            }
        }
    }

    #[cfg(feature = "xray")]
    async fn collect_user_metrics(&self) {
        let node_uuid = self.node.uuid;
        let base_tags = self.node.get_base_tags();

        let active_conns = {
            let mem = self.memory.read().await;

            mem.iter()
                .map(|(id, conn)| (*id, conn.get_subscription_id(), conn.get_proto().proto()))
                .collect::<Vec<_>>()
        };

        for (conn_id, subscription_id, proto) in active_conns {
            let res = self.conn(Prefix::ConnPrefix(conn_id)).await;
            match res {
                Ok(stats) => {
                    tracing::debug!("Successfully fetched stats for {}", conn_id);
                    let mut metric_tags = base_tags.clone();
                    metric_tags.insert("conn_id".to_string(), conn_id.to_string());
                    metric_tags.insert("proto".to_string(), proto.to_string());
                    if let Some(subscription_id) = subscription_id {
                        metric_tags
                            .insert("subscription_id".to_string(), subscription_id.to_string());
                    }

                    self.metrics.push(
                        node_uuid,
                        "user.traffic.downlink",
                        stats.downlink as f64,
                        metric_tags.clone(),
                    );
                    self.metrics.push(
                        node_uuid,
                        "user.traffic.uplink",
                        stats.uplink as f64,
                        metric_tags.clone(),
                    );
                    self.metrics.push(
                        node_uuid,
                        "user.traffic.online",
                        stats.online as f64,
                        metric_tags,
                    );
                }
                Err(e) => {
                    tracing::error!("Failed to get stats for user {}: {:?}", conn_id, e);
                }
            }
        }
    }

    #[cfg(feature = "wireguard")]
    async fn collect_wg_metrics(&self) {
        let wg_client = match &self.wg_client {
            Some(c) => c,
            None => return,
        };

        let node_uuid = self.node.uuid;
        let base_tags = self.node.get_base_tags();

        let wg_conns = {
            let mem = self.memory.read().await;
            mem.iter()
                .filter_map(|(id, conn)| {
                    conn.get_wireguard()
                        .map(|wg| (*id, conn.get_subscription_id(), wg.keys.pubkey()))
                })
                .collect::<Vec<_>>()
        };

        for (conn_id, subscription_id, pubkey) in wg_conns {
            if let Ok(pubkey) = pubkey {
                if let Ok((uplink, downlink, last_handshake_ms)) = wg_client.peer_stats(&pubkey) {
                    let mut metric_tags = base_tags.clone();
                    metric_tags.insert("conn_id".to_string(), conn_id.to_string());
                    metric_tags.insert("proto".to_string(), "wireguard".to_string());
                    if let Some(subscription_id) = subscription_id {
                        metric_tags
                            .insert("subscription_id".to_string(), subscription_id.to_string());
                    }
                    self.metrics.push(
                        node_uuid,
                        "user.traffic.downlink",
                        downlink as f64,
                        metric_tags.clone(),
                    );
                    self.metrics.push(
                        node_uuid,
                        "user.traffic.uplink",
                        uplink as f64,
                        metric_tags.clone(),
                    );
                    self.metrics.push(
                        node_uuid,
                        "user.traffic.online",
                        peer_online_by_handshake(last_handshake_ms),
                        metric_tags,
                    );
                }
            }
        }
    }

    #[cfg(feature = "amnezia-wg")]
    async fn collect_awg_metrics(&self) {
        let awg_client = match &self.awg_client {
            Some(c) => c,
            None => return,
        };

        let node_uuid = self.node.uuid;
        let base_tags = self.node.get_base_tags();

        let all_stats = match awg_client.peer_stats() {
            Ok(stats) => stats,
            Err(e) => {
                tracing::error!("Failed to read AmneziaWG peer stats: {}", e);
                return;
            }
        };

        let awg_conns = {
            let mem = self.memory.read().await;
            mem.iter()
                .filter_map(|(id, conn)| {
                    conn.get_amneziawg()
                        .map(|awg| (*id, conn.get_subscription_id(), awg.keys.pubkey()))
                })
                .collect::<Vec<_>>()
        };

        for (conn_id, subscription_id, pubkey) in awg_conns {
            let Ok(pubkey) = pubkey else {
                continue;
            };
            let Ok(decoded) = fcore::AwgInterface::decode_pubkey(&pubkey) else {
                continue;
            };
            let Some(stats) = all_stats.get(&decoded) else {
                continue;
            };

            let mut metric_tags = base_tags.clone();
            metric_tags.insert("conn_id".to_string(), conn_id.to_string());
            metric_tags.insert("proto".to_string(), "amneziawg".to_string());
            if let Some(subscription_id) = subscription_id {
                metric_tags.insert("subscription_id".to_string(), subscription_id.to_string());
            }

            self.metrics.push(
                node_uuid,
                "user.traffic.downlink",
                stats.tx_bytes as f64,
                metric_tags.clone(),
            );
            self.metrics.push(
                node_uuid,
                "user.traffic.uplink",
                stats.rx_bytes as f64,
                metric_tags.clone(),
            );
            self.metrics.push(
                node_uuid,
                "user.traffic.online",
                peer_online_by_handshake(stats.last_handshake.map(|ns| ns / 1_000_000)),
                metric_tags,
            );
        }
    }
}
