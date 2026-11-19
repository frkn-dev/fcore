//! In-memory state of the peers layer.
//!
//! Same shape as the rest of the api: memory is the hot copy used for every
//! validation, Postgres is durable storage. A handler writes the database
//! first, then updates these stores — never the other way round, otherwise
//! memory and database drift apart.
//!
//! Only one api instance is assumed (it is how the service runs today): two
//! instances would keep separate copies and one would not see peers created by
//! the other until a restart. The unique indexes in Postgres still prevent
//! double allocation; they just turn a race into a retry instead of a silent
//! duplicate.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use fcore::Env;

use crate::postgres::peers::{AccessRow, DeviceRow, PeerRow};

/// Live tunnels with the indexes validation needs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeerStore {
    by_id: HashMap<Uuid, PeerRow>,
    /// env → address → peer. One lookup answers "is this address free".
    by_env_addr: HashMap<Env, HashMap<Ipv4Addr, Uuid>>,
    /// (env, public key) → peer: idempotent creates and key collisions.
    by_env_key: HashMap<(Env, String), Uuid>,
    by_sub: HashMap<Uuid, HashSet<Uuid>>,
    by_device: HashMap<Uuid, HashSet<Uuid>>,
}

impl PeerStore {
    pub fn load(rows: Vec<PeerRow>) -> Self {
        let mut store = Self::default();
        for row in rows.into_iter().filter(|r| r.revoked_at.is_none()) {
            store.insert(row);
        }
        store
    }

    pub fn insert(&mut self, row: PeerRow) {
        if row.revoked_at.is_some() {
            return;
        }
        if let Ok(address) = row
            .address
            .split('/')
            .next()
            .unwrap_or("")
            .parse::<Ipv4Addr>()
        {
            self.by_env_addr
                .entry(row.env.clone())
                .or_default()
                .insert(address, row.id);
        }
        self.by_env_key
            .insert((row.env.clone(), row.public_key.clone()), row.id);
        self.by_sub
            .entry(row.subscription_id)
            .or_default()
            .insert(row.id);
        self.by_device
            .entry(row.device_id)
            .or_default()
            .insert(row.id);
        self.by_id.insert(row.id, row);
    }

    /// Revocation: the row leaves every index, so its address is free again
    /// and it no longer counts towards limits.
    pub fn remove_live(&mut self, id: Uuid) -> Option<PeerRow> {
        let row = self.by_id.remove(&id)?;
        if let Ok(address) = row
            .address
            .split('/')
            .next()
            .unwrap_or("")
            .parse::<Ipv4Addr>()
        {
            if let Some(map) = self.by_env_addr.get_mut(&row.env) {
                map.remove(&address);
                if map.is_empty() {
                    self.by_env_addr.remove(&row.env);
                }
            }
        }
        self.by_env_key
            .remove(&(row.env.clone(), row.public_key.clone()));
        if let Some(set) = self.by_sub.get_mut(&row.subscription_id) {
            set.remove(&id);
        }
        if let Some(set) = self.by_device.get_mut(&row.device_id) {
            set.remove(&id);
        }
        Some(row)
    }

    pub fn used_in(&self, env: &Env) -> HashSet<Ipv4Addr> {
        self.by_env_addr
            .get(env)
            .map(|map| map.keys().copied().collect())
            .unwrap_or_default()
    }

    pub fn live_by_key(&self, env: &Env, public_key: &str) -> Option<&PeerRow> {
        self.by_env_key
            .get(&(env.clone(), public_key.to_string()))
            .and_then(|id| self.by_id.get(id))
    }

    pub fn get(&self, id: &Uuid) -> Option<&PeerRow> {
        self.by_id.get(id)
    }

    /// `(tunnels of the device, tunnels of the subscription)`.
    pub fn counts(&self, subscription_id: &Uuid, device_id: &Uuid) -> (usize, usize) {
        (
            self.by_device.get(device_id).map_or(0, |s| s.len()),
            self.by_sub.get(subscription_id).map_or(0, |s| s.len()),
        )
    }

    pub fn live_of_sub(&self, subscription_id: &Uuid) -> Vec<&PeerRow> {
        self.by_sub
            .get(subscription_id)
            .map(|ids| ids.iter().filter_map(|id| self.by_id.get(id)).collect())
            .unwrap_or_default()
    }

    pub fn live_of_device(&self, device_id: &Uuid) -> Vec<&PeerRow> {
        self.by_device
            .get(device_id)
            .map(|ids| ids.iter().filter_map(|id| self.by_id.get(id)).collect())
            .unwrap_or_default()
    }

    /// Revoke every live tunnel of one subscription in one env (grant
    /// withdrawn or expired). Returns what was removed so the caller can
    /// publish a `Delete` for each of them.
    pub fn remove_env_of_sub(&mut self, subscription_id: &Uuid, env: &Env) -> Vec<PeerRow> {
        let ids: Vec<Uuid> = self
            .by_sub
            .get(subscription_id)
            .map(|ids| {
                ids.iter()
                    .filter(|id| self.by_id.get(id).is_some_and(|row| row.env == *env))
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        ids.into_iter()
            .filter_map(|id| self.remove_live(id))
            .collect()
    }

    /// Tunnels of one env: what a node of that env must carry.
    pub fn live_of_env(&self, env: &Env) -> Vec<&PeerRow> {
        self.by_env_addr
            .get(env)
            .map(|map| map.values().filter_map(|id| self.by_id.get(id)).collect())
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Devices with the two lookups the handlers need.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceStore {
    by_id: HashMap<Uuid, DeviceRow>,
    by_uid: HashMap<(Uuid, Uuid), Uuid>,
    by_support: HashMap<String, Uuid>,
}

impl DeviceStore {
    pub fn load(rows: Vec<DeviceRow>) -> Self {
        let mut store = Self::default();
        for row in rows {
            store.insert(row);
        }
        store
    }

    pub fn insert(&mut self, row: DeviceRow) {
        self.by_uid
            .insert((row.subscription_id, row.device_uid), row.id);
        self.by_support.insert(row.support_tag.clone(), row.id);
        self.by_id.insert(row.id, row);
    }

    pub fn by_uid(&self, subscription_id: &Uuid, device_uid: &Uuid) -> Option<&DeviceRow> {
        self.by_uid
            .get(&(*subscription_id, *device_uid))
            .and_then(|id| self.by_id.get(id))
    }

    pub fn by_support_tag(&self, tag: &str) -> Option<&DeviceRow> {
        self.by_support
            .get(&tag.to_uppercase())
            .and_then(|id| self.by_id.get(id))
    }

    pub fn get(&self, id: &Uuid) -> Option<&DeviceRow> {
        self.by_id.get(id)
    }

    pub fn of_sub(&self, subscription_id: &Uuid) -> Vec<&DeviceRow> {
        self.by_id
            .values()
            .filter(|row| row.subscription_id == *subscription_id)
            .collect()
    }
}

/// Grants keyed by `(subscription, env)` — one row per env, as in the table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccessStore {
    by_key: HashMap<(Uuid, String), AccessRow>,
    by_sub: HashMap<Uuid, HashSet<String>>,
}

impl AccessStore {
    pub fn load(rows: Vec<(Uuid, AccessRow)>) -> Self {
        let mut store = Self::default();
        for (subscription_id, row) in rows {
            store.insert(subscription_id, row);
        }
        store
    }

    pub fn insert(&mut self, subscription_id: Uuid, row: AccessRow) {
        let env = row.env.to_string();
        self.by_sub
            .entry(subscription_id)
            .or_default()
            .insert(env.clone());
        self.by_key.insert((subscription_id, env), row);
    }

    pub fn get(&self, subscription_id: &Uuid, env: &Env) -> Option<&AccessRow> {
        self.by_key.get(&(*subscription_id, env.to_string()))
    }

    pub fn of_sub(&self, subscription_id: &Uuid) -> Vec<&AccessRow> {
        self.by_sub
            .get(subscription_id)
            .map(|envs| {
                envs.iter()
                    .filter_map(|env| self.by_key.get(&(*subscription_id, env.clone())))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn is_empty_for(&self, subscription_id: &Uuid) -> bool {
        self.by_sub
            .get(subscription_id)
            .is_none_or(|s| s.is_empty())
    }

    /// Drops one grant (revoked or expired). Returns the row that left the
    /// hot copy so the caller can log or answer with it.
    pub fn remove(&mut self, subscription_id: &Uuid, env: &Env) -> Option<AccessRow> {
        let env = env.to_string();
        let row = self.by_key.remove(&(*subscription_id, env.clone()));
        if let Some(set) = self.by_sub.get_mut(subscription_id) {
            set.remove(&env);
            if set.is_empty() {
                self.by_sub.remove(subscription_id);
            }
        }
        row
    }
}

/// Everything the peers layer keeps in memory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeersState {
    pub peers: PeerStore,
    pub devices: DeviceStore,
    pub access: AccessStore,
    /// env → subnet blocks, as stored in `env_networks` ("10.88.0.0/24").
    pub env_networks: HashMap<Env, Vec<String>>,
    /// env → what the api declared about this shard.
    pub env_capacity: HashMap<Env, ShardCapacity>,
    /// Per-subscription overrides of `subscription_limits`:
    /// `(peers_per_device, peers_per_subscription)`. Absent row → deployment
    /// defaults from the config.
    pub limits: HashMap<Uuid, (Option<i64>, Option<i64>)>,
}

/// One `env_capacity` row as the api remembers it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardCapacity {
    pub plan: String,
    pub max_subscriptions: Option<i64>,
    pub enabled: bool,
    /// Human name from `env_capacity.title`; `null` for legacy rows.
    pub title: Option<String>,
    pub kind: crate::peers::ShardKind,
}

impl PeersState {
    /// Declared kind of a shard, `None` when the shard has no `env_capacity`
    /// row (legacy regions and envs created before shards existed).
    pub fn shard_kind(&self, env: &Env) -> Option<crate::peers::ShardKind> {
        self.env_capacity.get(env).map(|row| row.kind)
    }

    pub fn blocks_for(&self, env: &Env) -> Vec<&str> {
        self.env_networks
            .get(env)
            .map(|blocks| blocks.iter().map(|b| b.as_str()).collect())
            .unwrap_or_default()
    }

    /// Blocks of an env ready for the allocator. Unparsable rows are skipped:
    /// the table is seeded from the config, so a bad row is a config bug the
    /// startup log already names.
    pub fn block_masks(&self, env: &Env) -> Vec<fcore::IpAddrMask> {
        self.blocks_for(env)
            .into_iter()
            .filter_map(|block| block.parse().ok())
            .collect()
    }

    /// The live grant of `(subscription, env)`, if any.
    pub fn grant(&self, subscription_id: &Uuid, env: &Env) -> Option<&AccessRow> {
        self.access.get(subscription_id, env)
    }

    /// `(per_device, per_subscription)` overrides; `None` = use the defaults.
    pub fn limits_for(&self, subscription_id: &Uuid) -> (Option<i64>, Option<i64>) {
        self.limits
            .get(subscription_id)
            .copied()
            .unwrap_or((None, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peers::Limits;
    use chrono::Utc;

    fn peer(id: Uuid, env: Env, key: &str, addr: &str, sub: Uuid, dev: Uuid) -> PeerRow {
        PeerRow {
            id,
            device_id: dev,
            subscription_id: sub,
            env,
            proto: fcore::Tag::AmneziaPeer,
            public_key: key.to_string(),
            address: addr.to_string(),
            label: None,
            created_at: Utc::now(),
            revoked_at: None,
            revoke_reason: None,
        }
    }

    #[test]
    fn address_becomes_free_after_revocation() {
        let id = Uuid::new_v4();
        let mut store = PeerStore::load(vec![peer(
            id,
            Env::Ru,
            "KEY1",
            "10.88.0.2",
            Uuid::nil(),
            Uuid::nil(),
        )]);
        assert!(store
            .used_in(&Env::Ru)
            .contains(&"10.88.0.2".parse().unwrap()));
        store.remove_live(id);
        assert!(store.used_in(&Env::Ru).is_empty());
        assert_eq!(store.len(), 1 - 1);
    }

    #[test]
    fn counters_follow_limits() {
        let sub = Uuid::new_v4();
        let dev = Uuid::new_v4();
        let mut store = PeerStore::load(vec![
            peer(Uuid::new_v4(), Env::Ru, "K1", "10.88.0.2", sub, dev),
            peer(Uuid::new_v4(), Env::Ru, "K2", "10.88.0.3", sub, dev),
            peer(
                Uuid::new_v4(),
                Env::Ru,
                "K3",
                "10.88.0.4",
                sub,
                Uuid::new_v4(),
            ),
        ]);
        assert_eq!(store.counts(&sub, &dev), (2, 3));
        store.remove_live(store.live_of_sub(&sub)[0].id);
        assert_eq!(store.counts(&sub, &dev).1, 2);
    }

    #[test]
    fn key_lookup_is_per_env() {
        let store = PeerStore::load(vec![peer(
            Uuid::new_v4(),
            Env::Ru,
            "K1",
            "10.88.0.2",
            Uuid::nil(),
            Uuid::nil(),
        )]);
        assert!(store.live_by_key(&Env::Ru, "K1").is_some());
        assert!(store.live_by_key(&Env::Wl, "K1").is_none());
    }

    #[test]
    fn devices_are_found_by_uid_and_support_tag() {
        let sub = Uuid::new_v4();
        let uid = Uuid::new_v4();
        let store = DeviceStore::load(vec![DeviceRow {
            id: Uuid::new_v4(),
            subscription_id: sub,
            device_uid: uid,
            device_name: Some("iPhone".into()),
            public_key: "K".into(),
            platform: None,
            support_tag: "7K2M-9XQ4P".into(),
            created_at: Utc::now(),
            rotated_at: None,
        }]);
        assert!(store.by_uid(&sub, &uid).is_some());
        assert!(store.by_support_tag("7k2m-9xq4p").is_some());
        assert!(store.by_support_tag("NOPE").is_none());
    }

    fn access(sub: Uuid, env: Env) -> AccessRow {
        AccessRow {
            id: Uuid::new_v4(),
            env,
            plan: Some("standard".into()),
            protos: vec![fcore::Tag::AmneziaPeer],
            sources: vec!["payment".into()],
            expires_at: None,
            traffic_limit: None,
        }
    }

    #[test]
    fn a_grant_is_read_and_revoked_per_env() {
        let sub = Uuid::new_v4();
        let mut store = AccessStore::load(vec![
            (sub, access(sub, Env::Ru)),
            (sub, access(sub, Env::Wl)),
        ]);
        assert!(store.get(&sub, &Env::Ru).is_some());
        let gone = store.remove(&sub, &Env::Ru).expect("row removed");
        assert_eq!(gone.env, Env::Ru);
        assert!(store.get(&sub, &Env::Ru).is_none());
        // The other env keeps its grant, the subscription is not emptied.
        assert!(store.get(&sub, &Env::Wl).is_some());
        assert!(!store.is_empty_for(&sub));
        // Removing the last one drops the subscription index entry.
        store.remove(&sub, &Env::Wl);
        assert!(store.is_empty_for(&sub));
    }

    #[test]
    fn limits_fall_back_to_defaults_when_there_is_no_row() {
        let sub = Uuid::new_v4();
        let mut state = PeersState::default();
        assert_eq!(state.limits_for(&sub), (None, None));
        state.limits.insert(sub, (Some(10), None));
        assert_eq!(state.limits_for(&sub), (Some(10), None));
        assert_eq!(state.limits_for(&Uuid::new_v4()), (None, None));
    }

    #[test]
    fn blocks_seeded_from_config_are_parsed_and_garbage_is_skipped() {
        let mut state = PeersState::default();
        state
            .env_networks
            .insert(Env::Ru, vec!["10.88.0.0/24".into(), "not-a-cidr".into()]);
        let masks = state.block_masks(&Env::Ru);
        assert_eq!(masks.len(), 1);
        assert_eq!(masks[0].to_string(), "10.88.0.0/24");
        assert!(state.block_masks(&Env::Wl).is_empty());
    }

    #[test]
    fn revoking_a_grant_frees_addresses_of_that_env_only() {
        let sub = Uuid::new_v4();
        let device = Uuid::new_v4();
        let mut store = PeerStore::load(vec![
            peer(Uuid::new_v4(), Env::Ru, "K1", "10.88.0.2", sub, device),
            peer(Uuid::new_v4(), Env::Ru, "K2", "10.88.0.3", sub, device),
            peer(Uuid::new_v4(), Env::Wl, "K3", "10.89.0.2", sub, device),
        ]);
        let removed = store.remove_env_of_sub(&sub, &Env::Ru);
        assert_eq!(removed.len(), 2);
        assert!(store.used_in(&Env::Ru).is_empty());
        assert_eq!(store.used_in(&Env::Wl).len(), 1);
        assert_eq!(store.counts(&sub, &device), (1, 1));
        assert!(store.live_by_key(&Env::Ru, "K1").is_none());
        assert!(store.live_by_key(&Env::Wl, "K3").is_some());
    }

    #[test]
    fn a_written_through_peer_counts_and_occupies_its_address() {
        let sub = Uuid::new_v4();
        let device = Uuid::new_v4();
        let mut state = PeersState::default();
        let row = peer(Uuid::new_v4(), Env::Ru, "K1", "10.88.0.2", sub, device);
        state.peers.insert(row.clone());

        assert_eq!(state.peers.counts(&sub, &device), (1, 1));
        assert_eq!(state.peers.live_of_device(&device).len(), 1);
        assert!(state
            .peers
            .used_in(&Env::Ru)
            .contains(&"10.88.0.2".parse().unwrap()));
        assert_eq!(
            state.peers.live_by_key(&Env::Ru, "K1").map(|p| p.id),
            Some(row.id)
        );

        // A limit of one makes the next create fail before it touches the DB.
        let limits = Limits::resolve(
            Limits {
                per_device: 3,
                per_subscription: 45,
                devices_per_subscription: 15,
            },
            Some(1),
            None,
        );
        let (device_live, _) = state.peers.counts(&sub, &device);
        assert!(limits.check_device(device_live as i64).is_err());

        // Revocation gives the address back to the allocator.
        state.peers.remove_live(row.id);
        assert!(state.peers.used_in(&Env::Ru).is_empty());
        assert_eq!(state.peers.counts(&sub, &device), (0, 0));
    }
}

