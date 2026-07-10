use dashmap::DashMap;
use rkyv::Deserialize as RkyvDeserialize;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::Path;

use super::{MetricPoint};
use crate::error::Result;

#[derive(Clone, Debug, Default)]
pub struct WebMetricSample {
    pub name: String,
    pub tags: BTreeMap<String, String>,
    pub value: f64,
    pub timestamp_ms: i64,
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct WebMetricStorageSnapshot {
    pub series: Vec<WebMetricSeriesSnapshot>,
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct WebMetricSeriesSnapshot {
    pub metric: String,
    pub tags: BTreeMap<String, String>,
    pub points: Vec<MetricPoint>,
}

pub struct WebMetricStorage {
    pub inner: DashMap<String, DashMap<u64, VecDeque<MetricPoint>>>,
    pub metadata: DashMap<u64, BTreeMap<String, String>>,
    pub tag_index: DashMap<String, DashMap<String, HashSet<u64>>>,
    pub max_points: usize,
    pub retention_seconds: i64,
}

impl WebMetricStorage {
    pub fn new(max_points: usize, retention_seconds: i64) -> Self {
        Self {
            inner: DashMap::new(),
            metadata: DashMap::new(),
            tag_index: DashMap::new(),
            max_points,
            retention_seconds,
        }
    }

    pub fn insert(
        &self,
        metric: impl Into<String>,
        tags: BTreeMap<String, String>,
        value: f64,
        timestamp_ms: i64,
    ) {
        let metric = metric.into();
        let key = Self::make_series_key(&metric, &tags);

        self.metadata.entry(key).or_insert_with(|| {
            for (k, v) in &tags {
                self.tag_index
                    .entry(k.clone())
                    .or_default()
                    .entry(v.clone())
                    .or_default()
                    .insert(key);
            }
            tags.clone()
        });

        let metric_map = self.inner.entry(metric).or_default();
        let mut series = metric_map.entry(key).or_default();

        let min_ts = timestamp_ms - self.retention_seconds * 1000;

        if let Some(existing) = series.iter_mut().find(|p| p.timestamp == timestamp_ms) {
            existing.value = value;
        } else {
            series.push_back(MetricPoint {
                timestamp: timestamp_ms,
                value,
            });
        }

        while series.len() > self.max_points {
            series.pop_front();
        }

        while let Some(front) = series.front() {
            if front.timestamp < min_ts {
                series.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn insert_sample(&self, sample: WebMetricSample) {
        self.insert(sample.name, sample.tags, sample.value, sample.timestamp_ms);
    }

    pub fn make_series_key(metric: &str, tags: &BTreeMap<String, String>) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut h = DefaultHasher::new();
        metric.hash(&mut h);
        for (k, v) in tags {
            k.hash(&mut h);
            v.hash(&mut h);
        }
        h.finish()
    }

    pub fn series_for(
        &self,
        metric: &str,
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
        let metric_map = match self.inner.get(metric) {
            Some(m) => m,
            None => return HashSet::new(),
        };

        if tags.is_empty() {
            return metric_map.iter().map(|e| *e.key()).collect();
        }

        set.retain(|hash| metric_map.contains_key(hash));
        set
    }

    pub fn query_sum(
        &self,
        metric: &str,
        tags: &BTreeMap<String, String>,
        from_ms: i64,
        to_ms: i64,
    ) -> f64 {
        let hashes = self.series_for(metric, tags);
        let metric_map = match self.inner.get(metric) {
            Some(m) => m,
            None => return 0.0,
        };

        let mut total = 0.0;
        for hash in &hashes {
            if let Some(series) = metric_map.get(hash) {
                let sum: f64 = series
                    .iter()
                    .filter(|p| p.timestamp >= from_ms && p.timestamp <= to_ms)
                    .map(|p| p.value)
                    .sum();
                total += sum;
            }
        }
        total
    }

    pub fn query_points(
        &self,
        metric: &str,
        tags: &BTreeMap<String, String>,
        from_ms: i64,
        to_ms: i64,
    ) -> Vec<MetricPoint> {
        let hashes = self.series_for(metric, tags);
        let metric_map = match self.inner.get(metric) {
            Some(m) => m,
            None => return Vec::new(),
        };

        let mut points: Vec<MetricPoint> = Vec::new();
        for hash in &hashes {
            if let Some(series) = metric_map.get(hash) {
                points.extend(
                    series
                        .iter()
                        .filter(|p| p.timestamp >= from_ms && p.timestamp <= to_ms)
                        .cloned(),
                );
            }
        }
        points.sort_by_key(|p| p.timestamp);
        points
    }

    pub fn query_top(
        &self,
        metric: &str,
        tag_key: &str,
        from_ms: i64,
        to_ms: i64,
        limit: usize,
    ) -> Vec<(String, f64)> {
        let mut totals: BTreeMap<String, f64> = BTreeMap::new();

        let metric_map = match self.inner.get(metric) {
            Some(m) => m,
            None => return Vec::new(),
        };

        for entry in metric_map.iter() {
            let hash = *entry.key();
            let tags = match self.metadata.get(&hash) {
                Some(t) => t.clone(),
                None => continue,
            };

            let tag_value = match tags.get(tag_key) {
                Some(v) => v.clone(),
                None => continue,
            };

            let sum: f64 = entry
                .value()
                .iter()
                .filter(|p| p.timestamp >= from_ms && p.timestamp <= to_ms)
                .map(|p| p.value)
                .sum();

            *totals.entry(tag_value).or_insert(0.0) += sum;
        }

        let mut result: Vec<(String, f64)> = totals.into_iter().collect();
        result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        result.truncate(limit);
        result
    }

    pub fn perform_gc(&self) {
        let now = chrono::Utc::now().timestamp_millis();
        let min_ts = now - self.retention_seconds * 1000;

        self.inner.retain(|_, metric_map| {
            metric_map.retain(|_, series| {
                while let Some(front) = series.front() {
                    if front.timestamp < min_ts {
                        series.pop_front();
                    } else {
                        break;
                    }
                }
                !series.is_empty()
            });
            !metric_map.is_empty()
        });

        let mut alive = HashSet::new();
        for metric_map in self.inner.iter() {
            for hash in metric_map.value().iter().map(|e| *e.key()) {
                alive.insert(hash);
            }
        }

        self.metadata.retain(|k, _| alive.contains(k));
        self.tag_index.retain(|_, tag_map| {
            tag_map.retain(|_, set| {
                set.retain(|h| alive.contains(h));
                !set.is_empty()
            });
            !tag_map.is_empty()
        });
    }

    pub async fn save_snapshot<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        self.perform_gc();

        let base = path.as_ref();
        let tmp = base.with_extension("tmp");

        let mut series = Vec::new();
        for metric_map in self.inner.iter() {
            let metric = metric_map.key().clone();
            for entry in metric_map.iter() {
                let hash = *entry.key();
                let tags = match self.metadata.get(&hash) {
                    Some(t) => t.clone(),
                    None => continue,
                };
                let points: Vec<MetricPoint> = entry.value().iter().cloned().collect();
                series.push(WebMetricSeriesSnapshot {
                    metric: metric.clone(),
                    tags,
                    points,
                });
            }
        }

        let snapshot = WebMetricStorageSnapshot { series };
        let bytes = rkyv::to_bytes::<_, { 64 * 1024 * 1024 }>(&snapshot)?;

        tokio::fs::write(&tmp, bytes.as_slice()).await?;
        tokio::fs::rename(&tmp, base).await?;

        tracing::debug!("Saved web metrics snapshot ({} series)", snapshot.series.len());
        Ok(())
    }

    pub async fn load_snapshot<P: AsRef<Path>>(
        path: P,
        max_points: usize,
        retention_seconds: i64,
    ) -> Result<Self> {
        let bytes = tokio::fs::read(path).await?;
        let archived = unsafe { rkyv::archived_root::<WebMetricStorageSnapshot>(&bytes) };
        let snapshot: WebMetricStorageSnapshot = archived.deserialize(&mut rkyv::Infallible)?;

        let storage = Self::new(max_points, retention_seconds);
        for series in snapshot.series {
            storage.restore_series(series);
        }
        storage.perform_gc();
        Ok(storage)
    }

    fn restore_series(&self, series: WebMetricSeriesSnapshot) {
        let key = Self::make_series_key(&series.metric, &series.tags);

        self.metadata.insert(key, series.tags.clone());
        for (k, v) in &series.tags {
            self.tag_index
                .entry(k.clone())
                .or_default()
                .entry(v.clone())
                .or_default()
                .insert(key);
        }

        let metric_map = self.inner.entry(series.metric).or_default();
        let mut deque = VecDeque::new();
        for p in series.points {
            deque.push_back(p);
        }
        metric_map.insert(key, deque);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_query() {
        let storage = WebMetricStorage::new(1000, 86400);
        let mut tags = BTreeMap::new();
        tags.insert("page".to_string(), "/home".to_string());

        storage.insert("web.visits.page", tags.clone(), 1.0, 1000);
        storage.insert("web.visits.page", tags.clone(), 2.0, 2000);

        assert_eq!(storage.query_sum("web.visits.page", &BTreeMap::new(), 0, 3000), 3.0);
        assert_eq!(storage.query_sum("web.visits.page", &tags, 0, 3000), 3.0);
    }

    #[test]
    fn test_top() {
        let storage = WebMetricStorage::new(1000, 86400);

        let mut t1 = BTreeMap::new();
        t1.insert("page".to_string(), "/a".to_string());
        let mut t2 = BTreeMap::new();
        t2.insert("page".to_string(), "/b".to_string());

        storage.insert("web.visits.page", t1, 2.0, 1000);
        storage.insert("web.visits.page", t2, 5.0, 1000);

        let top = storage.query_top("web.visits.page", "page", 0, 2000, 10);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, "/b");
    }
}
