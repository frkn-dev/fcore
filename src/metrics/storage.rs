use dashmap::DashMap;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use super::{MetricEnvelope, MetricPoint};
use crate::memory::node::Node;
use crate::zmq::{publisher::Publisher, topic::Topic};

pub trait HasMetrics {
    fn metrics(&self) -> &MetricBuffer;
    fn node_settings(&self) -> &Node;
}

pub trait MetricSink {
    fn write(&self, node_id: &uuid::Uuid, metric: &str, value: f64, tags: BTreeMap<String, String>);
}

impl MetricSink for MetricBuffer {
    fn write(
        &self,
        node_id: &uuid::Uuid,
        metric: &str,
        value: f64,
        tags: BTreeMap<String, String>,
    ) {
        let mut b = self.batch.lock();

        let e = MetricEnvelope {
            node_id: *node_id,
            name: metric.to_string(),
            value,
            timestamp: chrono::Utc::now().timestamp_millis(),
            tags,
        };

        b.push(e.clone());
    }
}

pub struct MetricBuffer {
    pub batch: parking_lot::Mutex<Vec<MetricEnvelope>>,
    pub publisher: Publisher,
}

impl MetricBuffer {
    pub fn push(
        &self,
        node_id: uuid::Uuid,
        name: &str,
        value: f64,
        tags: BTreeMap<String, String>,
    ) {
        let mut b = self.batch.lock();
        b.push(MetricEnvelope {
            node_id,
            name: name.to_string(),
            value,
            timestamp: chrono::Utc::now().timestamp_millis(),
            tags,
        });
    }

    pub async fn flush_to_zmq(&self) {
        let metrics = {
            let mut batch = self.batch.lock();
            if batch.is_empty() {
                return;
            }
            std::mem::take(&mut *batch)
        };

        let bytes = rkyv::to_bytes::<_, 65536>(&metrics).expect("Failed to serialize batch");

        if let Err(e) = &self
            .publisher
            .send_binary(&Topic::Metrics, bytes.as_slice())
            .await
        {
            tracing::error!("Batch publish failed: {}", e);
        }
    }

    /// Return the current batch contents as Prometheus text exposition.
    /// The batch is read but not cleared, so regular ZMQ flushing continues.
    pub fn to_prometheus(&self, node_id: uuid::Uuid) -> String {
        let batch = self.batch.lock();
        let mut out = String::new();

        for e in batch.iter() {
            let name = sanitize_prometheus_name(&e.name);
            let mut tags = e.tags.clone();
            tags.insert("node_id".to_string(), node_id.to_string());
            let labels = format_prometheus_labels(&tags);

            out.push_str(&format!("{}{} {}\n", name, labels, e.value));
        }

        out
    }
}

fn sanitize_prometheus_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_alphanumeric() || c == ':' || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
        // Leading digit is illegal; prefix with underscore if needed.
        if i == 0 && c.is_ascii_digit() {
            out.insert(0, '_');
        }
    }
    if out.is_empty() {
        out.push_str("metric");
    }
    out
}

fn format_prometheus_labels(tags: &std::collections::BTreeMap<String, String>) -> String {
    if tags.is_empty() {
        return String::new();
    }

    let labels: Vec<String> = tags
        .iter()
        .map(|(k, v)| {
            format!(
                "{}=\"{}\"",
                sanitize_prometheus_name(k),
                escape_prometheus_label(v)
            )
        })
        .collect();

    format!("{{{}}}", labels.join(","))
}

fn escape_prometheus_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\"', "\\\"")
        .replace('\n', "\\n")
}

pub struct MetricStorage {
    pub inner: DashMap<uuid::Uuid, DashMap<u64, VecDeque<MetricPoint>>>,
    pub metadata: DashMap<u64, (String, BTreeMap<String, String>)>,
    pub tag_index: DashMap<String, DashMap<String, HashSet<u64>>>,

    pub max_points: usize,
    pub retention_seconds: i64,
}

impl MetricStorage {
    pub fn new(max_points: usize, retention_seconds: i64) -> Self {
        Self {
            inner: DashMap::new(),
            metadata: DashMap::new(),
            tag_index: DashMap::new(),
            max_points,
            retention_seconds,
        }
    }

    // ------------------------------------------------------------
    // INSERT
    // ------------------------------------------------------------

    pub fn insert_envelope(&self, e: MetricEnvelope) {
        let key = Self::make_series_key(&e.name, &e.tags);

        self.metadata.entry(key).or_insert_with(|| {
            for (k, v) in &e.tags {
                self.tag_index
                    .entry(k.clone())
                    .or_default()
                    .entry(v.clone())
                    .or_default()
                    .insert(key);
            }
            (e.name.clone(), e.tags.clone())
        });

        let node_map = self.inner.entry(e.node_id).or_default();
        let mut series = node_map.entry(key).or_default();

        series.push_back(MetricPoint {
            timestamp: e.timestamp,
            value: e.value,
        });

        while series.len() > self.max_points {
            series.pop_front();
        }

        let min_ts = e.timestamp - self.retention_seconds * 1000;

        while let Some(front) = series.front() {
            if front.timestamp < min_ts {
                series.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn make_series_key(name: &str, tags: &BTreeMap<String, String>) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut h = DefaultHasher::new();
        name.hash(&mut h);
        for (k, v) in tags {
            k.hash(&mut h);
            v.hash(&mut h);
        }
        h.finish()
    }

    // ------------------------------------------------------------
    // LOW LEVEL ACCESS
    // ------------------------------------------------------------

    pub fn metric_name(&self, hash: u64) -> Option<String> {
        self.metadata.get(&hash).map(|m| m.0.clone())
    }

    pub fn series_for(
        &self,
        metric: Option<&str>,
        tags: &BTreeMap<String, String>,
    ) -> HashSet<u64> {
        let mut result: Option<HashSet<u64>> = None;

        for (k, v) in tags {
            let current = self
                .tag_index
                .get(k)
                .and_then(|m| m.get(v).map(|x| x.clone()))
                .unwrap_or_default();

            result = Some(match result {
                None => current,
                Some(prev) => prev.intersection(&current).copied().collect(),
            });
        }

        let mut set = result.unwrap_or_default();

        if let Some(metric) = metric {
            set.retain(|hash| {
                self.metadata
                    .get(hash)
                    .map(|m| m.value().0 == metric)
                    .unwrap_or(false)
            });
        }

        set
    }

    // ------------------------------------------------------------
    // CORE QUERY API
    // ------------------------------------------------------------

    pub fn latest_sum(&self, metric: Option<&str>, tags: &BTreeMap<String, String>) -> f64 {
        let hashes = self.series_for(metric, tags);

        let mut total = 0.0;

        for node in self.inner.iter() {
            let node_map = node.value();

            for hash in &hashes {
                if let Some(series) = node_map.get(hash) {
                    if let Some(last) = series.back() {
                        total += last.value;
                    }
                }
            }
        }

        total
    }

    pub fn delta_sum(
        &self,
        metric: Option<&str>,
        tags: &BTreeMap<String, String>,
        from: i64,
        to: i64,
    ) -> f64 {
        let hashes = self.series_for(metric, tags);

        let mut total = 0.0;

        for node in self.inner.iter() {
            let _node_id = *node.key();
            let node_map = node.value();

            for hash in &hashes {
                let Some(series) = node_map.get(hash) else {
                    continue;
                };

                let points = series
                    .iter()
                    .filter(|p| p.timestamp >= from && p.timestamp <= to)
                    .cloned()
                    .collect::<Vec<_>>();

                if let (Some(first), Some(last)) = (points.first(), points.last()) {
                    total += (last.value - first.value).max(0.0);
                }
            }
        }

        total
    }

    pub fn query_points(
        &self,
        metric: Option<&str>,
        tags: &BTreeMap<String, String>,
        from: i64,
        to: i64,
    ) -> HashMap<uuid::Uuid, Vec<MetricPoint>> {
        let hashes = self.series_for(metric, tags);
        let mut result = HashMap::new();

        for node in self.inner.iter() {
            let node_id = *node.key();
            let node_map = node.value();

            let mut all = Vec::new();

            for hash in &hashes {
                let Some(series) = node_map.get(hash) else {
                    continue;
                };

                let mut pts: Vec<MetricPoint> = series
                    .iter()
                    .filter(|p| p.timestamp >= from && p.timestamp <= to)
                    .cloned()
                    .collect();

                all.append(&mut pts);
            }

            all.sort_by_key(|p| p.timestamp);

            if !all.is_empty() {
                result.insert(node_id, all);
            }
        }

        result
    }

    // ------------------------------------------------------------
    // RANGE
    // ------------------------------------------------------------

    pub fn get_range(
        &self,
        node_id: &uuid::Uuid,
        series_hash: u64,
        from: i64,
        to: i64,
    ) -> Vec<MetricPoint> {
        self.inner
            .get(node_id)
            .and_then(|node| {
                node.get(&series_hash).map(|dq| {
                    dq.iter()
                        .filter(|p| p.timestamp >= from && p.timestamp <= to)
                        .cloned()
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    // ------------------------------------------------------------
    // HEARTBEAT
    // ------------------------------------------------------------

    pub fn get_last_heartbeat(&self, node_id: &uuid::Uuid) -> Option<i64> {
        let node = self.inner.get(node_id)?;

        for entry in node.iter() {
            let hash = *entry.key();

            if let Some((name, _)) = self.metadata.get(&hash).map(|m| m.value().clone()) {
                if name == "sys.heartbeat" {
                    return entry.value().back().map(|p| p.timestamp);
                }
            }
        }

        None
    }

    // ------------------------------------------------------------
    // NODE SNAPSHOT HELPERS
    // ------------------------------------------------------------

    fn node_series_values<F>(&self, node_id: &uuid::Uuid, mut predicate: F) -> Vec<f64>
    where
        F: FnMut(&str) -> bool,
    {
        let mut values = Vec::new();
        let Some(node_map) = self.inner.get(node_id) else {
            return values;
        };

        for entry in node_map.iter() {
            let hash = *entry.key();
            let Some(meta) = self.metadata.get(&hash) else {
                continue;
            };
            let name = &meta.value().0;
            if predicate(name) {
                if let Some(last) = entry.value().back() {
                    values.push(last.value);
                }
            }
        }

        values
    }

    pub fn node_latest_value(&self, node_id: &uuid::Uuid, metric: &str) -> Option<f64> {
        let values = self.node_series_values(node_id, |name| name == metric);
        values.into_iter().next()
    }

    pub fn node_cpu_avg(&self, node_id: &uuid::Uuid) -> Option<f64> {
        let values = self.node_series_values(node_id, |name| name.starts_with("sys.cpu."));
        if values.is_empty() {
            return None;
        }
        let avg = values.iter().sum::<f64>() / values.len() as f64;
        Some((avg * 100.0).round() / 100.0)
    }

    pub fn node_memory(&self, node_id: &uuid::Uuid) -> (Option<u64>, Option<u64>) {
        let used = self
            .node_latest_value(node_id, "sys.mem_used")
            .map(|v| v.max(0.0) as u64);
        let total = self
            .node_latest_value(node_id, "sys.mem_total")
            .map(|v| v.max(0.0) as u64);
        (used, total)
    }

    pub fn node_disk_usage(&self, node_id: &uuid::Uuid, mount: &str) -> Option<f64> {
        self.node_latest_value(node_id, &format!("sys.disk.{}.usage_percent", mount))
    }

    pub fn node_network_traffic(&self, node_id: &uuid::Uuid) -> (Option<f64>, Option<f64>) {
        let rx_values = self.node_series_values(node_id, |name| {
            let parts: Vec<&str> = name.split('.').collect();
            parts.len() == 3 && parts[0] == "net" && parts[2] == "rx_bps"
        });
        let tx_values = self.node_series_values(node_id, |name| {
            let parts: Vec<&str> = name.split('.').collect();
            parts.len() == 3 && parts[0] == "net" && parts[2] == "tx_bps"
        });

        let rx = if rx_values.is_empty() {
            None
        } else {
            Some(rx_values.iter().sum::<f64>())
        };
        let tx = if tx_values.is_empty() {
            None
        } else {
            Some(tx_values.iter().sum::<f64>())
        };
        (rx, tx)
    }

    fn sum_metric(&self, metric: &str, tags: &BTreeMap<String, String>) -> u64 {
        let hashes = self.series_for(Some(metric), tags);

        let mut total = 0u64;

        for node in self.inner.iter() {
            let node_map = node.value();

            for hash in &hashes {
                let Some(series) = node_map.get(hash) else {
                    continue;
                };

                let Some(first) = series.front() else {
                    continue;
                };

                let Some(last) = series.back() else {
                    continue;
                };

                let delta = (last.value - first.value).max(0.0) as u64;
                total += delta;
            }
        }

        total
    }

    pub fn get_connection_total_traffic(&self, conn_id: &uuid::Uuid) -> (u64, u64) {
        let mut tags = BTreeMap::new();
        tags.insert("conn_id".to_string(), conn_id.to_string());

        (
            self.sum_metric("user.traffic.uplink", &tags),
            self.sum_metric("user.traffic.downlink", &tags),
        )
    }

    pub fn get_subscription_total_traffic(&self, subscription_id: &uuid::Uuid) -> (u64, u64) {
        let mut tags = BTreeMap::new();
        tags.insert("subscription_id".to_string(), subscription_id.to_string());

        (
            self.sum_metric("user.traffic.uplink", &tags),
            self.sum_metric("user.traffic.downlink", &tags),
        )
    }

    fn latest_value_sum(&self, metric: &str, tags: &BTreeMap<String, String>) -> u64 {
        self.latest_sum(Some(metric), tags).max(0.0) as u64
    }

    pub fn get_connection_online_count(&self, conn_id: &uuid::Uuid) -> u64 {
        let mut tags = BTreeMap::new();
        tags.insert("conn_id".to_string(), conn_id.to_string());
        self.latest_value_sum("user.traffic.online", &tags)
    }

    pub fn get_subscription_online_count(&self, subscription_id: &uuid::Uuid) -> u64 {
        let mut tags = BTreeMap::new();
        tags.insert("subscription_id".to_string(), subscription_id.to_string());
        self.latest_value_sum("user.traffic.online", &tags)
    }

    pub fn get_connection_delta_traffic(
        &self,
        conn_id: &uuid::Uuid,
        from_ms: i64,
        to_ms: i64,
    ) -> (u64, u64) {
        let mut tags = BTreeMap::new();
        tags.insert("conn_id".to_string(), conn_id.to_string());

        (
            self.delta_sum(Some("user.traffic.uplink"), &tags, from_ms, to_ms) as u64,
            self.delta_sum(Some("user.traffic.downlink"), &tags, from_ms, to_ms) as u64,
        )
    }

    pub fn reset_subscription_series(&self, subscription_id: &uuid::Uuid) {
        let sub_id_str = subscription_id.to_string();
        let hashes: std::collections::HashSet<u64> = self
            .tag_index
            .get("subscription_id")
            .and_then(|index| index.get(&sub_id_str).map(|entry| entry.clone()))
            .unwrap_or_default();

        let hashes: Vec<u64> = hashes.iter().copied().collect();

        if hashes.is_empty() {
            return;
        }

        for node in self.inner.iter() {
            let node_map = node.value();
            for hash in &hashes {
                if let Some(mut series) = node_map.get_mut(hash) {
                    while series.len() > 1 {
                        series.pop_front();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_sanitize_prometheus_name() {
        assert_eq!(sanitize_prometheus_name("sys.mem_free"), "sys_mem_free");
        assert_eq!(
            sanitize_prometheus_name("net.ens3.tx_bps"),
            "net_ens3_tx_bps"
        );
        assert_eq!(sanitize_prometheus_name("123metric"), "_123metric");
        assert_eq!(sanitize_prometheus_name(""), "metric");
    }

    #[test]
    fn test_format_prometheus_labels() {
        let mut tags = BTreeMap::new();
        tags.insert("hostname".to_string(), "darkmachine".to_string());
        tags.insert("env".to_string(), "experimental".to_string());
        let labels = format_prometheus_labels(&tags);
        assert!(labels.contains("hostname=\"darkmachine\""));
        assert!(labels.contains("env=\"experimental\""));
        assert!(labels.starts_with('{') && labels.ends_with('}'));
    }

    #[test]
    fn test_escape_prometheus_label() {
        assert_eq!(escape_prometheus_label("a\"b"), "a\\\"b");
        assert_eq!(escape_prometheus_label("a\\b"), "a\\\\b");
        assert_eq!(escape_prometheus_label("a\nb"), "a\\nb");
    }

    #[tokio::test]
    async fn test_metric_buffer_to_prometheus() {
        let publisher = Publisher::connect("inproc://test-metric-buffer-prometheus")
            .await
            .expect("connect publisher");
        let buffer = MetricBuffer {
            batch: parking_lot::Mutex::new(Vec::new()),
            publisher,
        };

        let node_id = uuid::Uuid::parse_str("ab514c21-aaaa-bbbb-cccc-32f8cb1ada40").unwrap();
        let mut tags = BTreeMap::new();
        tags.insert("hostname".to_string(), "test-node".to_string());

        buffer.push(node_id, "sys.mem_free", 1024.0, tags);

        let output = buffer.to_prometheus(node_id);
        assert!(output.contains("sys_mem_free"));
        assert!(output.contains("node_id=\"ab514c21-aaaa-bbbb-cccc-32f8cb1ada40\""));
        assert!(output.contains("hostname=\"test-node\""));
        assert!(output.contains("1024"));
    }
}
