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

    pub fn get_subscription_total_traffic(&self, subscription_id: &uuid::Uuid) -> (u64, u64) {
        let mut tags = BTreeMap::new();
        tags.insert("subscription_id".to_string(), subscription_id.to_string());

        let uplink = self.sum_metric("user.traffic.uplink", &tags);
        let downlink = self.sum_metric("user.traffic.downlink", &tags);

        (uplink, downlink)
    }
}
