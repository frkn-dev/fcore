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
    /// (env, pool, public key) → peer: idempotent creates and key collisions.
    /// The pool is part of the identity: one key may hold a tunnel in every
    /// pool of a shard (`POST /v1/peers/prepare`), while the same key twice in
    /// one pool is still a retry of the same tunnel.
    by_env_key: HashMap<(Env, Option<String>, String), Uuid>,
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
        self.by_env_key.insert(
            (row.env.clone(), row.pool.clone(), row.public_key.clone()),
            row.id,
        );
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
            .remove(&(row.env.clone(), row.pool.clone(), row.public_key.clone()));
        if let Some(set) = self.by_sub.get_mut(&row.subscription_id) {
            set.remove(&id);
        }
        if let Some(set) = self.by_device.get_mut(&row.device_id) {
            set.remove(&id);
        }
        Some(row)
    }

    /// Every live peer, cloned. Used by admin maintenance ("retire the whole
    /// shard") where iterating the indexes would borrow the store mutably.
    pub fn live(&self) -> Vec<PeerRow> {
        self.by_id.values().cloned().collect()
    }

    pub fn used_in(&self, env: &Env) -> HashSet<Ipv4Addr> {
        self.by_env_addr
            .get(env)
            .map(|map| map.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Live tunnel of `(env, pool, public key)`.
    ///
    /// This is the idempotency lookup of every create, so `pool` is part of the
    /// key: a device holds one tunnel per pool, and returning the tunnel of
    /// another pool would name a config the caller did not ask for. `None` is
    /// the manual form (`env` + `proto`) — it prefers an unpooled row and falls
    /// back to any pool of the env, which is how it behaved before pools
    /// became part of the identity.
    pub fn live_by_key_in(
        &self,
        env: &Env,
        pool: Option<&str>,
        public_key: &str,
    ) -> Option<&PeerRow> {
        if let Some(pool) = pool {
            return self
                .by_env_key
                .get(&(env.clone(), Some(pool.to_string()), public_key.to_string()))
                .and_then(|id| self.by_id.get(id));
        }
        self.live_by_key(env, public_key)
    }

    /// Any live tunnel with this key in the env: the unpooled row when there is
    /// one, otherwise any pool (the manual form's historical reading).
    pub fn live_by_key(&self, env: &Env, public_key: &str) -> Option<&PeerRow> {
        self.by_env_key
            .get(&(env.clone(), None, public_key.to_string()))
            .or_else(|| {
                self.by_env_key
                    .iter()
                    .find(|((row_env, _pool, key), _)| row_env == env && key == public_key)
                    .map(|(_, id)| id)
            })
            .and_then(|id| self.by_id.get(id))
    }

    pub fn get(&self, id: &Uuid) -> Option<&PeerRow> {
        self.by_id.get(id)
    }

    /// Ids of the live tunnels opened by one device row.
    pub fn ids_of_device(&self, device_id: &Uuid) -> Vec<Uuid> {
        self.by_device
            .get(device_id)
            .map(|ids| ids.iter().copied().collect())
            .unwrap_or_default()
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

    /// Live tunnels of one device row. Used by the per-device cap, which
    /// unions the rows of the same physical device (one per subscription).
    pub fn count_of_device(&self, device_id: &Uuid) -> usize {
        self.by_device.get(device_id).map_or(0, |ids| ids.len())
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
    /// Device public key → device rows. The X25519 key is the identity of the
    /// physical machine: the same phone on a second subscription gets a second
    /// row with the same key, and its tunnels must count as one device.
    by_key: HashMap<String, HashSet<Uuid>>,
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
        self.by_key
            .entry(row.public_key.clone())
            .or_default()
            .insert(row.id);
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

    /// Device rows that carry this X25519 key, across every subscription. A
    /// machine that bought a second subscription has one row per subscription
    /// and one key, so its tunnels must be counted together.
    pub fn ids_by_public_key(&self, key: &str) -> Vec<Uuid> {
        self.by_key
            .get(key)
            .map(|ids| ids.iter().copied().collect())
            .unwrap_or_default()
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

    /// Drops a device the reaping task deleted. Its peers are already revoked
    /// and therefore absent from `PeerStore`.
    pub fn remove(&mut self, id: &Uuid) -> Option<DeviceRow> {
        let row = self.by_id.remove(id)?;
        self.by_uid.remove(&(row.subscription_id, row.device_uid));
        self.by_support.remove(&row.support_tag);
        if let Some(ids) = self.by_key.get_mut(&row.public_key) {
            ids.remove(id);
            if ids.is_empty() {
                self.by_key.remove(&row.public_key);
            }
        }
        Some(row)
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
    /// env + pool → subnet blocks (`env_networks.pool` not null). Allocation
    /// prefers these over the env-wide block: an address then names its pool.
    pub pool_networks: HashMap<(Env, String), Vec<String>>,
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
        // The shard's own blocks plus every pool block of it: callers ask "is
        // this env served at all" (registration, capacity budget) and must see
        // the pool ranges too — after the /20 split the env-wide row is gone.
        let mut masks: Vec<fcore::IpAddrMask> = self
            .blocks_for(env)
            .into_iter()
            .filter_map(|block| block.parse().ok())
            .collect();
        let mut pool_masks: Vec<fcore::IpAddrMask> = Vec::new();
        for ((block_env, _pool), blocks) in &self.pool_networks {
            if block_env == env {
                pool_masks.extend(blocks.iter().filter_map(|block| block.parse().ok()));
            }
        }
        // Deterministic order: this list is also a fallback allocation pool, and
        // HashMap iteration order would hand out addresses from a random block.
        pool_masks.sort_by_key(|mask| mask.to_string());
        masks.extend(pool_masks);
        masks
    }

    /// Blocks the allocator uses for a peer of `(env, pool)`.
    ///
    /// A pool with its own block gets exactly that block; otherwise the env-wide
    /// blocks stay in play (every shard that has not been split yet). Addresses
    /// already issued from the env block remain valid — this only decides where
    /// NEW addresses come from.
    pub fn block_masks_for(&self, env: &Env, pool: Option<&str>) -> Vec<fcore::IpAddrMask> {
        if let Some(pool) = pool {
            if let Some(blocks) = self.pool_networks.get(&(env.clone(), pool.to_string())) {
                if !blocks.is_empty() {
                    return blocks.iter().filter_map(|block| block.parse().ok()).collect();
                }
            }
        }
        // Fallback is the shard-wide row only: a pool without its own block must
        // not start drawing addresses out of ANOTHER pool's /20.
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

    /// Live tunnels of the physical device that carries `device_key`, counted
    /// across every subscription and env.
    ///
    /// The key is `devices.public_key`: one installation keeps one X25519 pair,
    /// so the same machine on a second subscription is the same device and must
    /// not get a second allowance. Peers of the router's extra channels carry
    /// their own keys but belong to the same device rows, so they are counted
    /// too — otherwise the three-tunnel router case would never reach the cap.
    pub fn peers_of_device_key(&self, device_key: &str) -> i64 {
        self.devices
            .ids_by_public_key(device_key)
            .iter()
            .map(|id| self.peers.count_of_device(id) as i64)
            .sum()
    }

    /// Live tunnels of the physical device inside **one pool of one env**.
    ///
    /// This is the cap the client actually hits: three tunnels per pool, so a
    /// router can keep two channels up in the same pool for failover. Counting
    /// them across every pool (as `peers_of_device_key` does) made the third
    /// pool's tunnel refuse with `peers_limit` even though each pool had a free
    /// slot. Rows written before `peers.pool` existed have no pool: they are
    /// counted for any pool of their env, which is the stricter reading.
    pub fn peers_of_device_key_in(&self, device_key: &str, env: &Env, pool: &str) -> i64 {
        self.devices
            .ids_by_public_key(device_key)
            .iter()
            .flat_map(|id| self.peers.ids_of_device(id))
            .filter_map(|peer_id| self.peers.get(&peer_id))
            .filter(|row| &row.env == env)
            .filter(|row| match row.pool.as_deref() {
                Some(row_pool) => row_pool == pool,
                None => true,
            })
            .count() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peers::{Limits, Refusal};
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
            pool: None,
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

        // A pool with its own block is numbered inside that block; a pool
        // without one keeps the env-wide block, so nothing breaks on a shard
        // that has not been split yet.
        state.pool_networks.insert(
            (Env::Ru, "us".to_string()),
            vec!["10.92.0.0/16".to_string()],
        );
        let pool = state.block_masks_for(&Env::Ru, Some("us"));
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].to_string(), "10.92.0.0/16");
        let fallback = state.block_masks_for(&Env::Ru, Some("eu"));
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].to_string(), "10.88.0.0/24");
        // No pool named: the shard-wide row only (a pool-less caller must not
        // pick up another pool's block).
        let envwide = state.block_masks_for(&Env::Ru, None);
        assert_eq!(envwide.len(), 1);
        assert_eq!(envwide[0].to_string(), "10.88.0.0/24");

        // `block_masks(env)` is the "is this shard served" view: it must list
        // the env-wide block AND every pool block of the shard, otherwise a
        // shard that has been split into /20s looks empty to those callers.
        let served = state.block_masks(&Env::Ru);
        assert_eq!(served.len(), 2);
        assert!(served.iter().any(|m| m.to_string() == "10.92.0.0/16"));
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
        let mut dev_row = device_row(sub, "DEVKEY", "7K2M-9XQ4P");
        dev_row.id = device;
        state.devices.insert(dev_row);
        assert!(limits
            .check_device_peers(state.peers_of_device_key("DEVKEY"))
            .is_err());

        // Revocation gives the address back to the allocator.
        state.peers.remove_live(row.id);
        assert!(state.peers.used_in(&Env::Ru).is_empty());
        assert_eq!(state.peers.counts(&sub, &device), (0, 0));
        assert_eq!(state.peers_of_device_key("DEVKEY"), 0);
    }

    fn device_row(sub: Uuid, key: &str, tag: &str) -> DeviceRow {
        DeviceRow {
            id: Uuid::new_v4(),
            subscription_id: sub,
            device_uid: Uuid::new_v4(),
            device_name: Some("iPhone".into()),
            public_key: key.to_string(),
            platform: None,
            support_tag: tag.to_string(),
            created_at: Utc::now(),
            rotated_at: None,
        }
    }

    /// The cap is per physical device, not per subscription: a second
    /// subscription of the same machine (same X25519 key, its own device row)
    /// must not hand out another allowance. Counting per subscription would
    /// miss exactly this case — each subscription holds only one of the three
    /// tunnels.
    #[test]
    fn a_device_cap_counts_across_subscriptions_and_envs() {
        let sub_a = Uuid::new_v4();
        let sub_b = Uuid::new_v4();
        let mut state = PeersState::default();
        let dev_a = device_row(sub_a, "DEVKEY", "7K2M-9XQ4P");
        let dev_b = device_row(sub_b, "DEVKEY", "8QZ3-AAAAA");
        let id_a = dev_a.id;
        let id_b = dev_b.id;
        state.devices.insert(dev_a);
        state.devices.insert(dev_b);

        // Two tunnels on the first subscription, in two different envs.
        state.peers.insert(peer(
            Uuid::new_v4(),
            Env::Ru,
            "K1",
            "10.88.0.2",
            sub_a,
            id_a,
        ));
        state.peers.insert(peer(
            Uuid::new_v4(),
            Env::Wl,
            "K2",
            "10.89.0.2",
            sub_a,
            id_a,
        ));
        assert_eq!(state.peers_of_device_key("DEVKEY"), 2);

        // The third tunnel lives on the second subscription: the cap of three
        // is full even though neither subscription is near its own ceiling.
        state.peers.insert(peer(
            Uuid::new_v4(),
            Env::Ru,
            "K3",
            "10.88.0.3",
            sub_b,
            id_b,
        ));
        let limits = Limits {
            per_device: 3,
            per_subscription: 45,
            devices_per_subscription: 15,
        };
        assert_eq!(state.peers_of_device_key("DEVKEY"), 3);
        assert_eq!(
            limits.check_device_peers(state.peers_of_device_key("DEVKEY")),
            Err(Refusal::PeersLimit(3))
        );

        // Another device with its own key keeps its full allowance.
        state
            .devices
            .insert(device_row(sub_a, "OTHERKEY", "9ZZZ-BBBBB"));
        assert_eq!(state.peers_of_device_key("OTHERKEY"), 0);
        assert!(limits.check_device_peers(0).is_ok());
    }

    /// A router keeps three tunnels up on one device: extra channels carry
    /// their own peer keys but belong to the same device row, so they must be
    /// counted or the failover case would never reach the cap.
    #[test]
    fn extra_channels_of_one_device_count_towards_its_cap() {
        let sub = Uuid::new_v4();
        let mut state = PeersState::default();
        let device = device_row(sub, "DEVKEY", "7K2M-9XQ4P");
        let device_id = device.id;
        state.devices.insert(device);
        state.peers.insert(peer(
            Uuid::new_v4(),
            Env::Ru,
            "DEVKEY",
            "10.88.0.2",
            sub,
            device_id,
        ));
        // Second and third channels of the same router, own keys and labels.
        state.peers.insert(peer(
            Uuid::new_v4(),
            Env::Ru,
            "CHANNEL2",
            "10.88.0.3",
            sub,
            device_id,
        ));
        state.peers.insert(peer(
            Uuid::new_v4(),
            Env::Wl,
            "CHANNEL3",
            "10.89.0.2",
            sub,
            device_id,
        ));
        assert_eq!(state.peers_of_device_key("DEVKEY"), 3);
        assert_eq!(
            Limits {
                per_device: 3,
                per_subscription: 45,
                devices_per_subscription: 15,
            }
            .check_device_peers(state.peers_of_device_key("DEVKEY")),
            Err(Refusal::PeersLimit(3))
        );
    }

    /// Re-creating a tunnel for `(device, env)` reuses the live row: the count
    /// does not move, so at the cap a repeat of the same request is still the
    /// idempotent 200 the client relies on after a timeout.
    #[test]
    fn recreating_the_same_peer_does_not_add_to_the_device_count() {
        let sub = Uuid::new_v4();
        let mut state = PeersState::default();
        let device = device_row(sub, "DEVKEY", "7K2M-9XQ4P");
        let device_id = device.id;
        state.devices.insert(device);
        let row = peer(
            Uuid::new_v4(),
            Env::Ru,
            "DEVKEY",
            "10.88.0.2",
            sub,
            device_id,
        );
        state.peers.insert(row.clone());
        assert_eq!(state.peers_of_device_key("DEVKEY"), 1);

        // The handler finds this row by (env, public_key) and answers 200
        // without inserting; if it did insert the same row, the ids are equal
        // and the index sets stay the same size.
        let existing = state
            .peers
            .live_by_key(&Env::Ru, "DEVKEY")
            .map(|p| p.id)
            .expect("the live tunnel is found again");
        assert_eq!(existing, row.id);
        state.peers.insert(row);
        assert_eq!(state.peers_of_device_key("DEVKEY"), 1);
    }

    /// Revoked rows leave every index, so a tunnel the user deleted hands its
    /// slot back instead of holding the cap forever.
    #[test]
    fn revoked_peers_are_not_counted_against_the_device() {
        let sub = Uuid::new_v4();
        let mut state = PeersState::default();
        let device = device_row(sub, "DEVKEY", "7K2M-9XQ4P");
        let device_id = device.id;
        state.devices.insert(device);
        let first = peer(Uuid::new_v4(), Env::Ru, "K1", "10.88.0.2", sub, device_id);
        let second = peer(Uuid::new_v4(), Env::Wl, "K2", "10.89.0.2", sub, device_id);
        state.peers.insert(first.clone());
        state.peers.insert(second);
        assert_eq!(state.peers_of_device_key("DEVKEY"), 2);

        state.peers.remove_live(first.id);
        assert_eq!(state.peers_of_device_key("DEVKEY"), 1);
        assert!(state.peers.live_by_key(&Env::Ru, "K1").is_none());
    }
}

#[cfg(test)]
mod per_pool_cap_tests {
    use super::*;
    use chrono::Utc;

    fn peer_in_pool(pool: &str, addr: &str) -> PeerRow {
        PeerRow {
            id: Uuid::new_v4(),
            device_id: Uuid::nil(),
            subscription_id: Uuid::nil(),
            env: Env::Ru,
            proto: fcore::Tag::AmneziaPeer,
            public_key: "DEVKEY".to_string(),
            address: addr.to_string(),
            pool: Some(pool.to_string()),
            label: None,
            created_at: Utc::now(),
            revoked_at: None,
            revoke_reason: None,
        }
    }

    fn state_with(peers: Vec<PeerRow>) -> PeersState {
        let mut state = PeersState::default();
        state.devices.insert(DeviceRow {
            id: Uuid::nil(),
            subscription_id: Uuid::nil(),
            device_uid: Uuid::nil(),
            device_name: None,
            public_key: "DEVKEY".to_string(),
            platform: None,
            support_tag: "tag".to_string(),
            created_at: Utc::now(),
            rotated_at: None,
        });
        for row in peers {
            state.peers.insert(row);
        }
        state
    }

    /// Three tunnels in `eu` fill only the `eu` cap: the same device still has
    /// its own three in `ru` and in `prem`. Counting across pools was the bug —
    /// the fourth pool refused with `peers_limit` while every pool had a free
    /// slot.
    #[test]
    fn the_cap_is_per_pool_not_per_device() {
        let state = state_with(vec![
            peer_in_pool("eu", "10.88.0.2"),
            peer_in_pool("eu", "10.88.0.3"),
            peer_in_pool("eu", "10.88.0.4"),
            peer_in_pool("ru", "10.88.0.5"),
            peer_in_pool("prem", "10.88.0.6"),
        ]);

        assert_eq!(state.peers_of_device_key_in("DEVKEY", &Env::Ru, "eu"), 3);
        assert_eq!(state.peers_of_device_key_in("DEVKEY", &Env::Ru, "ru"), 1);
        assert_eq!(state.peers_of_device_key_in("DEVKEY", &Env::Ru, "prem"), 1);
        assert_eq!(state.peers_of_device_key_in("DEVKEY", &Env::Ru, "us"), 0);
        // The old, pool-blind counter still sees all five.
        assert_eq!(state.peers_of_device_key("DEVKEY"), 5);
    }

    /// A row written before `peers.pool` existed cannot be attributed: it is
    /// counted for any pool of its env, which is the stricter reading.
    #[test]
    fn a_legacy_row_counts_for_every_pool_of_its_env() {
        let mut legacy = peer_in_pool("eu", "10.88.0.9");
        legacy.pool = None;
        let state = state_with(vec![legacy, peer_in_pool("ru", "10.88.0.10")]);

        assert_eq!(state.peers_of_device_key_in("DEVKEY", &Env::Ru, "eu"), 1);
        assert_eq!(state.peers_of_device_key_in("DEVKEY", &Env::Ru, "ru"), 2);
        assert_eq!(state.peers_of_device_key_in("DEVKEY", &Env::Dev, "ru"), 0);
    }

    /// One key per pool is the batch shape: the same device key lives in `eu`
    /// and in `ru` at once, and the idempotent lookup tells them apart.
    #[test]
    fn one_key_can_hold_a_tunnel_in_every_pool() {
        let state = state_with(vec![
            peer_in_pool("eu", "10.88.0.2"),
            peer_in_pool("ru", "10.88.0.3"),
        ]);

        let eu = state.peers.live_by_key_in(&Env::Ru, Some("eu"), "DEVKEY");
        let ru = state.peers.live_by_key_in(&Env::Ru, Some("ru"), "DEVKEY");
        assert_eq!(eu.map(|p| p.address.as_str()), Some("10.88.0.2"));
        assert_eq!(ru.map(|p| p.address.as_str()), Some("10.88.0.3"));
        assert_ne!(eu.map(|p| p.id), ru.map(|p| p.id));

        // Another pool of the same env is still empty, and another env too.
        assert!(state.peers.live_by_key_in(&Env::Ru, Some("us"), "DEVKEY").is_none());
        assert!(state.peers.live_by_key_in(&Env::Dev, Some("eu"), "DEVKEY").is_none());

        // Revoking one pool leaves the other, and frees the key for that pool.
        let eu_id = eu.unwrap().id;
        let mut state = state;
        state.peers.remove_live(eu_id);
        assert!(state.peers.live_by_key_in(&Env::Ru, Some("eu"), "DEVKEY").is_none());
        assert!(state.peers.live_by_key_in(&Env::Ru, Some("ru"), "DEVKEY").is_some());
    }
}
