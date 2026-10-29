//! Приём метрик и наблюдений о входах от нод.

use rkyv::Deserialize;
use std::net::IpAddr;
use std::sync::Arc;

use fcore::{EntryEnvelope, MetricEnvelope, MetricStorage, Subscriber};

use crate::postgres::usage::{EntryRow, PgUsage};

pub struct MetricWorker;

/// Соль для хеша адреса входа. Отдельная от телеметрии блокировок: это другой
/// набор данных, и общий хеш позволил бы сопоставлять их напрямую.
const ENTRY_SALT: &[u8] = b"frkn-entry-usage-v1";

impl MetricWorker {
    pub async fn start(metric_storage: Arc<MetricStorage>, subscriber: Subscriber, usage: PgUsage) {
        tracing::info!("MetricWorker: Monitoring pipeline initialized...");

        loop {
            if let Some((topic, payload_bytes)) = subscriber.recv().await {
                // `recv` отдаёт сырые байты топика — сравниваем с "entries".
                if topic.as_slice() == b"entries" {
                    Self::handle_entries(&usage, &payload_bytes).await;
                } else {
                    Self::handle_metrics(&metric_storage, &payload_bytes);
                }
            }
        }
    }

    fn handle_metrics(metric_storage: &MetricStorage, payload_bytes: &[u8]) {
        let archived = match rkyv::check_archived_root::<Vec<MetricEnvelope>>(payload_bytes) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("MetricWorker: Binary corruption detected: {:?}", e);
                return;
            }
        };

        let metrics: Vec<MetricEnvelope> = match archived.deserialize(&mut rkyv::Infallible) {
            Ok(m) => m,
            Err(e) => {
                tracing::error!("MetricWorker: Failed to reconstruct envelopes: {:?}", e);
                return;
            }
        };

        for metric in metrics {
            tracing::debug!(
                target: "metrics",
                "Incoming: node={} metric={} value={} tags={:?}",
                metric.node_id,
                metric.name,
                metric.value,
                metric.tags
            );

            metric_storage.insert_envelope(metric);
        }
    }

    /// Входы: адрес клиента → префикс + ASN/страна + хеш. Полный адрес не
    /// храним — только `/24` (или `/48` для IPv6) и соль+sha256.
    async fn handle_entries(usage: &PgUsage, payload_bytes: &[u8]) {
        let archived = match rkyv::check_archived_root::<Vec<EntryEnvelope>>(payload_bytes) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("EntryWorker: Binary corruption detected: {:?}", e);
                return;
            }
        };

        let entries: Vec<EntryEnvelope> = match archived.deserialize(&mut rkyv::Infallible) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("EntryWorker: Failed to reconstruct envelopes: {:?}", e);
                return;
            }
        };

        let now = chrono::Utc::now();
        let mut rows = Vec::with_capacity(entries.len());

        for entry in entries {
            let Ok(ip) = entry.ip.parse::<IpAddr>() else {
                continue;
            };
            let Some(prefix) = prefix_of(ip) else {
                continue;
            };
            let (asn, country) = fcore::geoip::lookup(ip);

            rows.push(EntryRow {
                subscription_id: entry.subscription_id,
                node_id: entry.node_id,
                proto: Some(entry.proto),
                entry_prefix: prefix,
                asn: asn.map(|v| v as i32),
                country,
                ip_hash: Some(ip_hash(ip)),
            });
        }

        if rows.is_empty() {
            return;
        }

        match usage.record_entry(&rows, now).await {
            Ok(n) => tracing::debug!("entry usage: wrote {n} rows"),
            Err(e) => tracing::error!("entry usage: write failed: {e:?}"),
        }
    }
}

/// `/24` для IPv4, `/48` для IPv6 — как в телеметрии блокировок.
fn prefix_of(ip: IpAddr) -> Option<String> {
    match ip {
        IpAddr::V4(v4) => {
            let mut seg = v4.octets();
            seg[3] = 0;
            Some(format!("{}/24", std::net::Ipv4Addr::from(seg)))
        }
        IpAddr::V6(v6) => {
            let mut seg = v6.segments();
            seg[3] = 0;
            seg[4] = 0;
            seg[5] = 0;
            seg[6] = 0;
            seg[7] = 0;
            Some(format!("{}/48", std::net::Ipv6Addr::from(seg)))
        }
    }
}

fn ip_hash(ip: IpAddr) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(ENTRY_SALT);
    hasher.update(ip.to_string().as_bytes());
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_v4_is_24() {
        assert_eq!(
            prefix_of("203.0.113.77".parse().unwrap()),
            Some("203.0.113.0/24".to_string())
        );
    }

    #[test]
    fn prefix_v6_is_48() {
        assert_eq!(
            prefix_of("2001:db8:abcd:1234::1".parse().unwrap()),
            Some("2001:db8:abcd::/48".to_string())
        );
    }

    #[test]
    fn hash_is_stable_and_does_not_leak_address() {
        let ip: IpAddr = "203.0.113.77".parse().unwrap();
        let a = ip_hash(ip);
        let b = ip_hash(ip);
        assert_eq!(a, b, "хеш должен быть детерминированным");
        assert_eq!(a.len(), 16, "8 байт в hex");
        assert!(!a.contains("203"), "адрес не должен утекать в хеш");

        let other: IpAddr = "203.0.113.78".parse().unwrap();
        assert_ne!(a, ip_hash(other), "разные адреса — разные хеши");
    }

    #[test]
    fn neighbours_share_prefix_but_not_hash() {
        let a: IpAddr = "203.0.113.77".parse().unwrap();
        let b: IpAddr = "203.0.113.200".parse().unwrap();
        assert_eq!(prefix_of(a), prefix_of(b));
        assert_ne!(ip_hash(a), ip_hash(b));
    }
}
