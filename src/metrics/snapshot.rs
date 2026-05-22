use rkyv::Deserialize;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::collections::VecDeque;

use super::{storage::MetricStorage, MetricPoint};
use crate::error::Result;

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct MetricStorageSnapshot {
    pub series: Vec<MetricSeriesSnapshot>,
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct MetricSeriesSnapshot {
    pub node_id: uuid::Uuid,
    pub metric: String,
    pub tags: BTreeMap<String, String>,
    pub points: Vec<MetricPoint>,
}

impl MetricStorage {
    // ------------------------------------------------------------
    // GC
    // ------------------------------------------------------------

    pub fn perform_gc(&self) {
        let now = chrono::Utc::now().timestamp_millis();
        let min_ts = now - self.retention_seconds * 1000;

        self.inner.retain(|_, node_map| {
            node_map.retain(|_, series| {
                while let Some(front) = series.front() {
                    if front.timestamp < min_ts {
                        series.pop_front();
                    } else {
                        break;
                    }
                }
                !series.is_empty()
            });

            !node_map.is_empty()
        });

        let mut alive = HashSet::new();

        for node in self.inner.iter() {
            for hash in node.value().iter().map(|e| *e.key()) {
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

    pub fn compact_series(
        points: &VecDeque<MetricPoint>,
        interval_ms: i64,
    ) -> VecDeque<MetricPoint> {
        if points.len() < 2 {
            return points.clone();
        }

        let mut result = VecDeque::new();

        let mut current_bucket: Option<i64> = None;
        let mut last_point: Option<MetricPoint> = None;

        for p in points {
            let bucket = p.timestamp / interval_ms;

            match current_bucket {
                None => {
                    current_bucket = Some(bucket);
                    last_point = Some(p.clone());
                }
                Some(b) if b == bucket => {
                    last_point = Some(p.clone());
                }
                Some(_) => {
                    if let Some(lp) = last_point.take() {
                        result.push_back(lp);
                    }

                    current_bucket = Some(bucket);
                    last_point = Some(p.clone());
                }
            }
        }

        if let Some(lp) = last_point {
            result.push_back(lp);
        }

        result
    }

    pub fn compact_old_points(&self, older_than_sec: i64, interval_sec: i64) {
        let now = chrono::Utc::now().timestamp_millis();

        let threshold = now - older_than_sec * 1000;
        let interval_ms = interval_sec * 1000;

        for mut node in self.inner.iter_mut() {
            for mut series in node.value_mut().iter_mut() {
                let original = series.value();

                if original.len() < 100 {
                    continue;
                }

                let mut old = VecDeque::new();
                let mut recent = VecDeque::new();

                for p in original.iter() {
                    if p.timestamp < threshold {
                        old.push_back(p.clone());
                    } else {
                        recent.push_back(p.clone());
                    }
                }

                let mut compacted = Self::compact_series(&old, interval_ms);

                compacted.extend(recent);

                *series.value_mut() = compacted;
            }
        }
    }
    pub fn snapshot(&self) -> MetricStorageSnapshot {
        fn should_persist(metric: &str) -> bool {
            matches!(
                metric,
                "user.traffic.uplink"
                    | "user.traffic.downlink"
                    | "user.traffic.online"
                    | "net.inbound.VlessTcpReality.uplink"
                    | "net.inbound.VlessTcpReality.downlink"
                    | "net.inbound.VlessXhttpReality.uplink"
                    | "net.inbound.VlessXhttpReality.downlink"
                    | "net.inbound.VlessGrpcReality.uplink"
                    | "net.inbound.VlessGrpcReality.downlink"
            )
        }

        let mut series = Vec::new();

        for node in self.inner.iter() {
            let node_id = *node.key();

            for entry in node.value().iter() {
                let hash = *entry.key();

                let Some(meta) = self.metadata.get(&hash) else {
                    continue;
                };

                let (metric, tags) = meta.value();

                if !should_persist(metric) {
                    continue;
                }

                series.push(MetricSeriesSnapshot {
                    node_id,
                    metric: metric.clone(),
                    tags: tags.clone(),
                    points: entry.value().iter().cloned().collect(),
                });
            }
        }

        MetricStorageSnapshot { series }
    }

    pub fn restore(snapshot: MetricStorageSnapshot, max_points: usize, retention_sec: i64) -> Self {
        let storage = Self::new(max_points, retention_sec);

        for series in snapshot.series {
            let hash = Self::make_series_key(&series.metric, &series.tags);

            storage
                .metadata
                .insert(hash, (series.metric.clone(), series.tags.clone()));

            for (k, v) in &series.tags {
                storage
                    .tag_index
                    .entry(k.clone())
                    .or_default()
                    .entry(v.clone())
                    .or_default()
                    .insert(hash);
            }

            let node = storage.inner.entry(series.node_id).or_default();

            let mut deque = VecDeque::new();

            for p in series.points {
                deque.push_back(p);
            }

            node.insert(hash, deque);
        }

        storage
    }

    pub async fn save_snapshot<P: AsRef<std::path::Path>>(&self, path: P) -> Result<()> {
        self.perform_gc();
        self.compact_old_points(3600, 60);

        let snapshot = self.snapshot();

        let points: usize = snapshot.series.iter().map(|s| s.points.len()).sum();

        tracing::debug!(
            "Saved {} metric series ({} points)",
            snapshot.series.len(),
            points
        );

        let bytes = rkyv::to_bytes::<_, { 8 * 1024 * 1024 }>(&snapshot)?;

        tracing::debug!(
            "Metric snapshot size: {:.2} MB",
            bytes.len() as f64 / 1024.0 / 1024.0
        );
        let tmp = path.as_ref().with_extension("tmp");
        tokio::fs::write(&tmp, bytes.as_slice()).await?;
        tokio::fs::rename(tmp, path).await?;
        let count = snapshot.series.len();
        tracing::info!("Saved {} metric series", count);

        Ok(())
    }

    pub async fn load_snapshot<P: AsRef<std::path::Path>>(
        path: P,
        max_points: usize,
        retention_sec: i64,
    ) -> Result<Self> {
        let bytes = tokio::fs::read(path).await?;

        let archived = unsafe { rkyv::archived_root::<MetricStorageSnapshot>(&bytes) };
        let snapshot: MetricStorageSnapshot = archived.deserialize(&mut rkyv::Infallible)?;
        let count = snapshot.series.len();
        tracing::info!("Restoring {} metric series", count);
        let storage = Self::restore(snapshot, max_points, retention_sec);
        storage.perform_gc();

        Ok(storage)
    }
}
