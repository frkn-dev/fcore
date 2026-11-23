//! Node metrics and entry observations.

use chrono::{DateTime, TimeDelta, Utc};
use rkyv::Deserialize;
use std::net::IpAddr;
use std::sync::Arc;
use uuid::Uuid;

use fcore::{EntryEnvelope, MetricEnvelope, MetricStorage, Subscriber, Topic};

use crate::postgres::{
    connection::{InstallationSample, PgConn},
    usage::{EntryRow, PgUsage},
};

pub struct MetricWorker;

/// Entry address hashes use a different salt so they cannot be correlated
/// directly with block observations.
const ENTRY_SALT: &[u8] = b"frkn-entry-usage-v1";

impl MetricWorker {
    pub async fn start(
        metric_storage: Arc<MetricStorage>,
        subscriber: Subscriber,
        usage: PgUsage,
        installation_observer: Option<(PgConn, i64)>,
    ) {
        tracing::info!("MetricWorker: Monitoring pipeline initialized...");

        loop {
            if let Some((topic, payload_bytes)) = subscriber.recv().await {
                if topic.as_slice() == Topic::Entries.as_str().as_bytes() {
                    Self::handle_entries(&usage, &payload_bytes).await;
                } else {
                    let observer = installation_observer
                        .as_ref()
                        .filter(|_| topic.as_slice() == Topic::Metrics.as_str().as_bytes());
                    let samples =
                        Self::handle_metrics(&metric_storage, &payload_bytes, observer.is_some());
                    if let Some((db, max_gap_ms)) = observer.filter(|_| !samples.is_empty()) {
                        if let Err(error) =
                            db.record_installation_samples(&samples, *max_gap_ms).await
                        {
                            tracing::warn!(%error, "Failed to persist installation activity samples");
                        }
                    }
                }
            }
        }
    }

    fn handle_metrics(
        metric_storage: &MetricStorage,
        payload_bytes: &[u8],
        observe_installations: bool,
    ) -> Vec<InstallationSample> {
        let archived = match rkyv::check_archived_root::<Vec<MetricEnvelope>>(payload_bytes) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("MetricWorker: Binary corruption detected: {:?}", e);
                return Vec::new();
            }
        };

        let metrics: Vec<MetricEnvelope> = match archived.deserialize(&mut rkyv::Infallible) {
            Ok(m) => m,
            Err(e) => {
                tracing::error!("MetricWorker: Failed to reconstruct envelopes: {:?}", e);
                return Vec::new();
            }
        };

        let observed_at = observe_installations.then(Utc::now);
        let mut samples = Vec::new();
        for metric in metrics {
            tracing::debug!(
                target: "metrics",
                "Incoming: node={} metric={} value={} tags={:?}",
                metric.node_id,
                metric.name,
                metric.value,
                metric.tags
            );

            if let Some(sample) =
                observed_at.and_then(|observed_at| installation_sample(&metric, observed_at))
            {
                samples.push(sample);
            }
            metric_storage.insert_envelope(metric);
        }
        samples
    }

    /// Record the client prefix, ASN, country and salted address hash.
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

fn installation_sample(
    metric: &MetricEnvelope,
    received_at: DateTime<Utc>,
) -> Option<InstallationSample> {
    if metric.name != "user.traffic.online"
        || !matches!(
            metric.tags.get("proto").map(String::as_str),
            Some("amneziawg" | "amneziawg_mobile")
        )
    {
        return None;
    }
    let online = match metric.value {
        0.0 => false,
        1.0 => true,
        _ => return None,
    };
    let conn_id = metric.tags.get("conn_id")?.parse::<Uuid>().ok()?;
    let sampled_at = DateTime::from_timestamp_millis(metric.timestamp)?;
    if conn_id.is_nil() || metric.node_id.is_nil() || metric.timestamp <= 0 {
        return None;
    }
    // Allow small clock differences without writing an observation from the future.
    if sampled_at - received_at > TimeDelta::seconds(30) {
        return None;
    }
    Some(InstallationSample {
        conn_id,
        node_id: metric.node_id,
        sampled_at: sampled_at.min(received_at),
        online,
    })
}

/// Match the prefix lengths used by block observations.
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
    fn installation_samples_only_use_tagged_awg_handshake_metrics() {
        let now = Utc::now();
        let conn_id = Uuid::new_v4();
        let mut metric = MetricEnvelope {
            node_id: Uuid::new_v4(),
            name: "user.traffic.online".to_string(),
            value: 1.0,
            timestamp: (now - TimeDelta::seconds(1)).timestamp_millis(),
            tags: [
                ("conn_id".to_string(), conn_id.to_string()),
                ("proto".to_string(), "amneziawg".to_string()),
            ]
            .into(),
        };
        let online = installation_sample(&metric, now).unwrap();
        assert!(online.online);
        assert_eq!(online.conn_id, conn_id);
        assert_eq!(online.node_id, metric.node_id);
        assert_eq!(online.sampled_at.timestamp_millis(), metric.timestamp);

        metric.value = 0.0;
        metric
            .tags
            .insert("proto".into(), "amneziawg_mobile".into());
        assert!(!installation_sample(&metric, now).unwrap().online);

        metric.value = 0.5;
        assert!(installation_sample(&metric, now).is_none());
        metric.value = f64::NAN;
        assert!(installation_sample(&metric, now).is_none());
        metric.value = 1.0;
        metric.tags.insert("proto".into(), "wireguard".into());
        assert!(installation_sample(&metric, now).is_none());
        metric
            .tags
            .insert("proto".into(), "amneziawg_mobile".into());
        metric.name = "user.traffic.uplink".to_string();
        assert!(installation_sample(&metric, now).is_none());
        metric.name = "user.traffic.online".to_string();
        metric.tags.remove("conn_id");
        assert!(installation_sample(&metric, now).is_none());
    }

    #[test]
    fn installation_samples_reject_invalid_times_and_bound_clock_skew() {
        let now = Utc::now();
        let mut metric = MetricEnvelope {
            node_id: Uuid::new_v4(),
            name: "user.traffic.online".to_string(),
            value: 1.0,
            timestamp: (now + TimeDelta::seconds(10)).timestamp_millis(),
            tags: [
                ("conn_id".to_string(), Uuid::new_v4().to_string()),
                ("proto".to_string(), "amneziawg".to_string()),
            ]
            .into(),
        };
        assert_eq!(installation_sample(&metric, now).unwrap().sampled_at, now);

        for timestamp in [
            0,
            -1,
            i64::MAX,
            (now + TimeDelta::minutes(1)).timestamp_millis(),
        ] {
            metric.timestamp = timestamp;
            assert!(installation_sample(&metric, now).is_none());
        }
    }

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
