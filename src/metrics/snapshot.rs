use rkyv::Deserialize;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;

use super::{storage::MetricStorage, MetricPoint};
use crate::error::Result;

const SNAPSHOT_PART_SIZE: usize = 300 * 1024 * 1024; // 300 MB

#[derive(rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
pub struct MetricStorageSnapshotMeta {
    pub version: u32,
    pub parts: usize,
}

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
                | "net.inbound.VlessXhttpCdn.uplink"
                | "net.inbound.VlessXhttpCdn.downlink"
                | "net.inbound.VlessGrpcReality.uplink"
                | "net.inbound.VlessGrpcReality.downlink"
        )
    }

    fn restore_series(&self, series: MetricSeriesSnapshot) {
        let hash = Self::make_series_key(&series.metric, &series.tags);

        self.metadata
            .insert(hash, (series.metric.clone(), series.tags.clone()));

        for (k, v) in &series.tags {
            self.tag_index
                .entry(k.clone())
                .or_default()
                .entry(v.clone())
                .or_default()
                .insert(hash);
        }

        let node = self.inner.entry(series.node_id).or_default();

        let mut deque = VecDeque::new();

        for p in series.points {
            deque.push_back(p);
        }

        node.insert(hash, deque);
    }

    async fn save_part(base: &Path, idx: usize, series: Vec<MetricSeriesSnapshot>) -> Result<()> {
        let snapshot = MetricStorageSnapshot { series };

        let bytes = rkyv::to_bytes::<_, { 64 * 1024 * 1024 }>(&snapshot)?;

        tracing::debug!(
            "Saving snapshot part {} ({:.2} MB)",
            idx,
            bytes.len() as f64 / 1024.0 / 1024.0
        );

        let path = base.with_extension(format!("part{}", idx));

        let tmp = path.with_extension("tmp");

        tokio::fs::write(&tmp, bytes.as_slice()).await?;

        tokio::fs::rename(tmp, path).await?;

        Ok(())
    }

    pub async fn save_snapshot<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        self.perform_gc();

        self.compact_old_points(3600, 60);

        let base = path.as_ref();

        // cleanup old parts
        let mut cleanup_idx = 0usize;

        loop {
            let old_part = base.with_extension(format!("part{}", cleanup_idx));

            if tokio::fs::metadata(&old_part).await.is_err() {
                break;
            }

            let _ = tokio::fs::remove_file(&old_part).await;

            cleanup_idx += 1;
        }

        let mut current_part = Vec::new();

        let mut current_size = 0usize;

        let mut part_idx = 0usize;

        let mut total_series = 0usize;

        let mut total_points = 0usize;

        for node in self.inner.iter() {
            let node_id = *node.key();

            for entry in node.value().iter() {
                let hash = *entry.key();

                let Some(meta) = self.metadata.get(&hash) else {
                    continue;
                };

                let (metric, tags) = meta.value();

                if !Self::should_persist(metric) {
                    continue;
                }

                let points: Vec<_> = entry.value().iter().cloned().collect();

                let estimated_size =
                    points.len() * std::mem::size_of::<MetricPoint>() + metric.len() + 2048;

                if current_size >= SNAPSHOT_PART_SIZE && !current_part.is_empty() {
                    Self::save_part(base, part_idx, current_part).await?;

                    current_part = Vec::new();

                    current_size = 0;

                    part_idx += 1;
                }

                total_points += points.len();

                total_series += 1;

                current_part.push(MetricSeriesSnapshot {
                    node_id,
                    metric: metric.clone(),
                    tags: tags.clone(),
                    points,
                });

                current_size += estimated_size;
            }
        }

        if !current_part.is_empty() {
            Self::save_part(base, part_idx, current_part).await?;

            part_idx += 1;
        }

        let meta = MetricStorageSnapshotMeta {
            version: 1,
            parts: part_idx,
        };

        let meta_bytes = rkyv::to_bytes::<_, { 1024 * 1024 }>(&meta)?;

        tokio::fs::write(base.with_extension("meta"), meta_bytes.as_slice()).await?;

        tracing::debug!(
            "Saved {} series ({} points) in {} parts",
            total_series,
            total_points,
            part_idx
        );

        Ok(())
    }

    pub async fn load_snapshot<P: AsRef<Path>>(
        path: P,
        max_points: usize,
        retention_sec: i64,
    ) -> Result<Self> {
        let base = path.as_ref();

        let meta_bytes = tokio::fs::read(base.with_extension("meta")).await?;

        let archived_meta =
            unsafe { rkyv::archived_root::<MetricStorageSnapshotMeta>(&meta_bytes) };

        let meta: MetricStorageSnapshotMeta = archived_meta.deserialize(&mut rkyv::Infallible)?;

        tracing::debug!("Loading metric snapshot ({} parts)", meta.parts);

        let storage = Self::new(max_points, retention_sec);

        for idx in 0..meta.parts {
            let path = base.with_extension(format!("part{}", idx));

            match tokio::fs::read(&path).await {
                Ok(bytes) => {
                    let archived = unsafe { rkyv::archived_root::<MetricStorageSnapshot>(&bytes) };

                    let snapshot: MetricStorageSnapshot =
                        archived.deserialize(&mut rkyv::Infallible)?;

                    for series in snapshot.series {
                        storage.restore_series(series);
                    }

                    tracing::debug!("Loaded snapshot part {}", idx);
                }

                Err(err) => {
                    tracing::warn!("Skipping missing/corrupted snapshot part {}: {}", idx, err);
                }
            }
        }

        storage.perform_gc();

        Ok(storage)
    }
}
