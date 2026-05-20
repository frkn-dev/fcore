use super::{storage::MetricStorage, MetricPoint};
use rkyv::Deserialize;
use std::collections::VecDeque;

use std::collections::BTreeMap;

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
        let snapshot = self.snapshot();

        let bytes = rkyv::to_bytes::<_, { 8 * 1024 * 1024 }>(&snapshot)?;
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
