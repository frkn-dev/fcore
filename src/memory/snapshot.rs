use chrono::Utc;
use rkyv::with::AsOwned;
use rkyv::with::With;
use rkyv::Infallible;
use rkyv::{to_bytes, Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;
use tokio::fs as async_fs;
use tokio::sync::RwLock;

use crate::error::{Error, Result};

use super::connection::conn::Conn;
use super::connection::operation::base::Operations as ConnectionBaseOperations;
use super::connection::Connections;
use super::tag::ProtoTag as Tag;

#[derive(Archive, Deserialize, Serialize, SerdeDeserialize, SerdeSerialize, Debug, Clone)]
#[archive(check_bytes)]
pub struct SnapshotData<C>
where
    C: Archive + Send + Sync + Clone + 'static,
{
    pub timestamp: u64,
    pub memory: Connections<C>,
    /// 2 since the file carries its identity (host/env/pool) and per-tag counts.
    pub version: u32,
    /// Host that wrote the file — diagnostics only, never fatal.
    pub hostname: String,
    pub env: String,
    pub pool: Option<String>,
    /// tag -> connections of that tag in this file. The startup check compares
    /// these with the api's count so a stale file is rejected before it can
    /// touch the interfaces.
    pub counts: BTreeMap<String, u64>,
}

/// Who the node is, for validating a snapshot before it is applied.
#[derive(Debug, Clone, Default)]
pub struct SnapshotIdentity {
    pub hostname: String,
    pub env: String,
    pub pool: Option<String>,
}

impl SnapshotIdentity {
    /// `Some(reason)` when the file belongs to a different node identity.
    ///
    /// A different env or pool is fatal by design: replacing a snapshot by hand
    /// is a supported operation, so a mismatch means the operator put a foreign
    /// file in place and the node must say so instead of serving it silently.
    pub fn mismatch(&self, meta: &SnapshotMeta) -> Option<String> {
        if !self.env.is_empty() && meta.env != self.env {
            return Some(format!(
                "snapshot env '{}' != node env '{}'",
                meta.env, self.env
            ));
        }
        if self.pool != meta.pool {
            return Some(format!(
                "snapshot pool {:?} != node pool {:?}",
                meta.pool, self.pool
            ));
        }
        None
    }
}

/// Metadata of a loaded snapshot (everything except the connections).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SnapshotMeta {
    pub version: u32,
    pub timestamp: u64,
    pub hostname: String,
    pub env: String,
    pub pool: Option<String>,
    pub counts: BTreeMap<String, u64>,
}

pub struct SnapshotManager<C>
where
    C: Archive + Send + Sync + Clone + 'static,
{
    pub snapshot_path: String,
    pub memory: Arc<RwLock<C>>,
    /// Written into every snapshot and checked on load.
    pub identity: SnapshotIdentity,
    /// Wakes the write-through task: the snapshot must reflect an applied
    /// Create/Update/Delete immediately, not at the next interval tick.
    pub wake: Arc<tokio::sync::Notify>,
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
            identity: self.identity.clone(),
            wake: self.wake.clone(),
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
        + ConnectionBaseOperations
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
            identity: SnapshotIdentity::default(),
            wake: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Identity used to stamp the file and validate it on load.
    pub fn with_identity(mut self, identity: SnapshotIdentity) -> Self {
        self.identity = identity;
        self
    }

    /// Share the wake handle with the node, so applying a message can request a
    /// write-through save from anywhere.
    pub fn with_wake(mut self, wake: Arc<tokio::sync::Notify>) -> Self {
        self.wake = wake;
        self
    }

    /// Ask the write-through task to persist the current state.
    pub fn request_save(&self) {
        self.wake.notify_one();
    }

    /// Per-tag counts of the in-memory state, `tag -> n`.
    pub fn counts(&self, conns: &Connections<C>) -> BTreeMap<String, u64>
    where
        C: ConnectionBaseOperations,
    {
        let mut out: BTreeMap<String, u64> = BTreeMap::new();
        for (_, conn) in conns.iter() {
            *out.entry(format!("{:?}", conn.get_proto().proto()))
                .or_insert(0) += 1;
        }
        out
    }

    pub async fn create_snapshot(&self) -> Result<()> {
        let memory_guard = self.memory.read().await;
        if memory_guard.is_empty() {
            drop(memory_guard);
            if Path::new(&self.snapshot_path).exists() {
                async_fs::remove_file(&self.snapshot_path).await.ok();
            }
            return Ok(());
        }
        let memory = memory_guard.clone();
        let timestamp = Utc::now().timestamp() as u64;
        let counts = self.counts(&memory);
        drop(memory_guard);

        let snapshot = SnapshotData {
            timestamp,
            memory,
            version: 2,
            hostname: self.identity.hostname.clone(),
            env: self.identity.env.clone(),
            pool: self.identity.pool.clone(),
            counts,
        };

        let bytes = to_bytes::<_, 256>(&snapshot)?;

        let temp_path = format!("{}.tmp", self.snapshot_path);
        async_fs::write(&temp_path, &bytes).await?;
        async_fs::rename(&temp_path, &self.snapshot_path).await?;

        Ok(())
    }

    pub async fn load_snapshot(&self) -> Result<Option<SnapshotMeta>>
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
                let meta = SnapshotMeta {
                    version: snapshot.version,
                    timestamp: snapshot.timestamp,
                    hostname: snapshot.hostname.clone(),
                    env: snapshot.env.clone(),
                    pool: snapshot.pool.clone(),
                    counts: snapshot.counts.clone(),
                };
                // The identity check happens before the state is applied: a file
                // from another env/pool must not touch the interfaces at all.
                if let Some(reason) = self.identity.mismatch(&meta) {
                    return Err(Error::Custom(format!(
                        "snapshot identity mismatch: {reason} ({}). Use --reset-snapshot or remove the file.",
                        self.snapshot_path
                    )));
                }
                if !meta.hostname.is_empty()
                    && !self.identity.hostname.is_empty()
                    && meta.hostname != self.identity.hostname
                {
                    tracing::warn!(
                        "snapshot was written by {} (this node is {})",
                        meta.hostname,
                        self.identity.hostname
                    );
                }

                let mut memory_guard = self.memory.write().await;
                memory_guard.0 = snapshot.memory.0.clone();
                Ok(Some(meta))
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

    pub async fn sync_cursor(&self, proto: Tag) -> Option<u64>
    where
        C: ConnectionBaseOperations,
    {
        let mem = self.memory.read().await;
        sync_cursor(&mem, proto)
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

pub fn sync_cursor<C>(conns: &Connections<C>, proto: Tag) -> Option<u64>
where
    C: ConnectionBaseOperations,
{
    conns
        .iter()
        .filter(|(_, conn)| !conn.get_deleted() && conn.get_proto().proto() == proto)
        .map(|(_, conn)| conn.get_modified_at().timestamp())
        .filter(|ts| *ts >= 0)
        .max()
        .map(|ts| ts as u64)
}

impl<C: Send + Sync + Clone + From<Conn>> Connections<C> {
    pub fn load_from_cache(&mut self, connections: Connections<C>) -> Result<()> {
        *self = connections;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::connection::base::Base;
    use super::super::connection::proto::Proto;
    use super::*;
    use chrono::{DateTime, Utc};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn cursor_is_per_proto_and_ignores_empty_and_deleted() {
        let mut conns = Connections::<Base>::default();
        assert_eq!(sync_cursor(&conns, Tag::VlessTcpReality), None);

        let mut tcp = Base::new(Proto::Xray(Tag::VlessTcpReality), None, None);
        tcp.modified_at = at(100);
        let mut tcp_older = Base::new(Proto::Xray(Tag::VlessTcpReality), None, None);
        tcp_older.modified_at = at(40);
        let mut grpc = Base::new(Proto::Xray(Tag::VlessGrpcReality), None, None);
        grpc.modified_at = at(70);
        let mut gone = Base::new(Proto::Xray(Tag::Vmess), None, None);
        gone.modified_at = at(500);
        gone.is_deleted = true;

        conns.insert(uuid::Uuid::new_v4(), tcp);
        conns.insert(uuid::Uuid::new_v4(), tcp_older);
        conns.insert(uuid::Uuid::new_v4(), grpc);
        conns.insert(uuid::Uuid::new_v4(), gone);

        assert_eq!(sync_cursor(&conns, Tag::VlessTcpReality), Some(100));
        assert_eq!(sync_cursor(&conns, Tag::VlessGrpcReality), Some(70));
        assert_eq!(sync_cursor(&conns, Tag::Vmess), None);
        assert_eq!(sync_cursor(&conns, Tag::Wireguard), None);
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    fn meta(env: &str, pool: Option<&str>) -> SnapshotMeta {
        SnapshotMeta {
            version: 2,
            timestamp: 1,
            hostname: "msk-tw.frkn.org".to_string(),
            env: env.to_string(),
            pool: pool.map(str::to_string),
            counts: BTreeMap::new(),
        }
    }

    #[test]
    fn same_identity_passes() {
        let identity = SnapshotIdentity {
            hostname: "msk-tw.frkn.org".to_string(),
            env: "dev".to_string(),
            pool: Some("RU Рилзопровод".to_string()),
        };
        assert!(identity.mismatch(&meta("dev", Some("RU Рилзопровод"))).is_none());
    }

    #[test]
    fn foreign_env_is_fatal() {
        // Manual snapshot replacement is supported, so a foreign file must stop
        // the node instead of being served silently.
        let identity = SnapshotIdentity {
            hostname: "msk-tw.frkn.org".to_string(),
            env: "dev".to_string(),
            pool: None,
        };
        let reason = identity.mismatch(&meta("wl", None)).expect("env mismatch");
        assert!(reason.contains("env"));
    }

    #[test]
    fn foreign_pool_is_fatal() {
        let identity = SnapshotIdentity {
            hostname: "msk-tw.frkn.org".to_string(),
            env: "dev".to_string(),
            pool: Some("RU Рилзопровод".to_string()),
        };
        let reason = identity
            .mismatch(&meta("dev", Some("EU Рилзопровод")))
            .expect("pool mismatch");
        assert!(reason.contains("pool"));
    }

    #[test]
    fn hostname_alone_is_not_fatal() {
        let identity = SnapshotIdentity {
            hostname: "other.host".to_string(),
            env: "dev".to_string(),
            pool: None,
        };
        assert!(identity.mismatch(&meta("dev", None)).is_none());
    }
}
