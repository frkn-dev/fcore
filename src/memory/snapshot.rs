use chrono::Utc;
use rkyv::with::AsOwned;
use rkyv::with::With;
use rkyv::Infallible;
use rkyv::{to_bytes, Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;
use tokio::fs as async_fs;
use tokio::sync::RwLock;

use crate::error::{Error, Result};

use super::connection::conn::Conn;
use super::connection::Connections;

#[derive(Archive, Deserialize, Serialize, SerdeDeserialize, SerdeSerialize, Debug, Clone)]
#[archive(check_bytes)]
pub struct SnapshotData<C>
where
    C: Archive + Send + Sync + Clone + 'static,
{
    pub timestamp: u64,
    pub memory: Connections<C>,
    pub version: u32,
}

pub struct SnapshotManager<C>
where
    C: Archive + Send + Sync + Clone + 'static,
{
    pub snapshot_path: String,
    pub memory: Arc<RwLock<C>>,
}

impl<C> Clone for SnapshotManager<C>
where
    C: Archive
        + Send
        + Sync
        + Clone
        + 'static
        + rkyv::Serialize<
            rkyv::ser::serializers::CompositeSerializer<
                rkyv::ser::serializers::AlignedSerializer<rkyv::AlignedVec>,
                rkyv::ser::serializers::FallbackScratch<
                    rkyv::ser::serializers::HeapScratch<256>,
                    rkyv::ser::serializers::AllocScratch,
                >,
                rkyv::ser::serializers::SharedSerializeMap,
            >,
        >,
{
    fn clone(&self) -> Self {
        SnapshotManager {
            snapshot_path: self.snapshot_path.clone(),
            memory: self.memory.clone(),
        }
    }
}

impl<C> SnapshotManager<Connections<C>>
where
    C: Archive
        + Send
        + Sync
        + Clone
        + 'static
        + std::convert::From<Conn>
        + rkyv::Serialize<
            rkyv::ser::serializers::CompositeSerializer<
                rkyv::ser::serializers::AlignedSerializer<rkyv::AlignedVec>,
                rkyv::ser::serializers::FallbackScratch<
                    rkyv::ser::serializers::HeapScratch<256>,
                    rkyv::ser::serializers::AllocScratch,
                >,
                rkyv::ser::serializers::SharedSerializeMap,
            >,
        >,
{
    pub fn new(snapshot_path: String, memory: Arc<RwLock<Connections<C>>>) -> Self {
        Self {
            snapshot_path,
            memory,
        }
    }

    pub async fn create_snapshot(&self) -> Result<()> {
        let memory_guard = self.memory.read().await;
        let memory = memory_guard.clone();
        let timestamp = Utc::now().timestamp() as u64;
        drop(memory_guard);

        let snapshot = SnapshotData {
            timestamp,
            memory,
            version: 1,
        };

        let bytes = to_bytes::<_, 256>(&snapshot)?;

        let temp_path = format!("{}.tmp", self.snapshot_path);
        async_fs::write(&temp_path, &bytes).await?;
        async_fs::rename(&temp_path, &self.snapshot_path).await?;

        Ok(())
    }

    pub async fn load_snapshot(&self) -> Result<Option<u64>>
    where
        <C as Archive>::Archived: rkyv::Deserialize<C, rkyv::Infallible>,
    {
        if !Path::new(&self.snapshot_path).exists() {
            return Ok(None);
        }

        let path = self.snapshot_path.clone();
        let snapshot = tokio::task::spawn_blocking(move || -> Result<Option<SnapshotData<C>>> {
            let bytes = std::fs::read(&path)?;
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let archived = unsafe { rkyv::archived_root::<SnapshotData<C>>(&bytes) };
                let with: With<SnapshotData<C>, AsOwned> = archived.deserialize(&mut Infallible)?;
                let snapshot: SnapshotData<C> = with.into_inner();
                Ok::<_, Error>(snapshot)
            }));

            match result {
                Ok(Ok(snapshot)) => Ok(Some(snapshot)),
                Ok(Err(e)) => Err(e),
                Err(_) => {
                    let backup = format!("{}.incompatible", path);
                    tracing::warn!(
                        "Snapshot {} appears incompatible, moving to {}",
                        path,
                        backup
                    );
                    std::fs::rename(&path, &backup).ok();
                    Ok(None)
                }
            }
        })
        .await
        .map_err(|e| Error::Custom(format!("Snapshot load task failed: {}", e)))??;

        match snapshot {
            Some(snapshot) => {
                let mut memory_guard = self.memory.write().await;
                memory_guard.0 = snapshot.memory.0.clone();
                Ok(Some(snapshot.timestamp))
            }
            None => Ok(None),
        }
    }

    pub async fn get_snapshot_timestamp(&self) -> Result<Option<u64>> {
        if !Path::new(&self.snapshot_path).exists() {
            return Ok(None);
        }

        let path = self.snapshot_path.clone();
        let timestamp = tokio::task::spawn_blocking(move || -> Result<Option<u64>> {
            let bytes = std::fs::read(&path)?;
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let archived = unsafe { rkyv::archived_root::<SnapshotData<C>>(&bytes) };
                archived.timestamp
            }));

            match result {
                Ok(ts) => Ok(Some(ts)),
                Err(_) => {
                    tracing::warn!(
                        "Snapshot {} appears incompatible, cannot read timestamp",
                        path
                    );
                    Ok(None)
                }
            }
        })
        .await
        .map_err(|e| Error::Custom(format!("Snapshot timestamp task failed: {}", e)))??;

        Ok(timestamp)
    }

    pub async fn len(&self) -> usize {
        let mem = self.memory.read().await;
        mem.0.len()
    }

    pub async fn is_empty(&self) -> bool {
        let mem = self.memory.read().await;
        mem.0.len() == 0
    }
}

impl<C: Send + Sync + Clone + From<Conn>> Connections<C> {
    pub fn load_from_cache(&mut self, connections: Connections<C>) -> Result<()> {
        *self = connections;
        Ok(())
    }
}
