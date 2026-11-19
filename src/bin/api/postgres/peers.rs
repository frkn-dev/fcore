//! Peer storage: grants (`subscription_access`), devices, tunnels (`peers`),
//! env blocks (`env_networks`) and the per-subscription limit overrides.
//!
//! Raw `tokio_postgres` queries, like the rest of `postgres/`. Every write
//! that can lose a race uses `ON CONFLICT DO NOTHING` and reports "nothing
//! inserted" instead of an error: the caller decides whether that means
//! "take the next address" or "this key is already live".

use std::net::Ipv4Addr;
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::pg::PgClientManager;

use fcore::{Env, IpAddrMask, Result, Tag};

/// One row of `subscription_access`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AccessRow {
    pub id: Uuid,
    pub env: Env,
    /// Which plan bought this row; `None` for manual/support grants.
    pub plan: Option<String>,
    pub protos: Vec<Tag>,
    pub sources: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub traffic_limit: Option<i64>,
}

/// One row of `devices`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DeviceRow {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub device_uid: Uuid,
    pub device_name: Option<String>,
    pub public_key: String,
    pub platform: Option<String>,
    pub support_tag: String,
    pub created_at: DateTime<Utc>,
}

/// One row of `peers`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PeerRow {
    pub id: Uuid,
    pub device_id: Uuid,
    pub subscription_id: Uuid,
    pub env: Env,
    pub proto: Tag,
    pub public_key: String,
    pub address: String,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
}

/// What an insert actually did. `Conflict` means the unique index refused it;
/// the caller retries with the next address or reports a taken key/label.
#[derive(Clone, Debug)]
pub enum Inserted {
    Done,
    Conflict,
}

pub struct PgPeers {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgPeers {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    /// Load the whole peers layer in one pass: what the api keeps in memory
    /// and validates against. Called once at startup, next to the connections
    /// and subscriptions load.
    pub async fn load_state(&self) -> Result<crate::peers::store::PeersState> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let peer_rows = client
            .query(
                &format!(
                    "SELECT {} FROM peers WHERE revoked_at IS NULL",
                    PEER_COLUMNS
                ),
                &[],
            )
            .await?;
        let peers = peer_rows.iter().map(peer_from_row).collect::<Vec<_>>();

        let device_rows = client
            .query(
                r#"
                SELECT id, subscription_id, device_uid, device_name, public_key, platform,
                       support_tag, created_at
                FROM devices
                "#,
                &[],
            )
            .await?;
        let devices = device_rows.iter().map(device_from_row).collect::<Vec<_>>();

        let grant_rows = client
            .query(
                r#"
                SELECT subscription_id, id, env, plan, protos, sources, expires_at, traffic_limit
                FROM subscription_access
                WHERE revoked_at IS NULL
                "#,
                &[],
            )
            .await?;
        let grants = grant_rows
            .iter()
            .map(|row| (row.get::<_, Uuid>("subscription_id"), access_from_row(row)))
            .collect::<Vec<_>>();

        let network_rows = client
            .query("SELECT env, subnet::text AS subnet FROM env_networks ORDER BY env, subnet", &[])
            .await?;

        let capacity_rows = client
            .query("SELECT env, plan, max_subscriptions, enabled FROM env_capacity", &[])
            .await?;

        let limit_rows = client
            .query(
                r#"
                SELECT subscription_id, peers_per_device, peers_per_subscription
                FROM subscription_limits
                "#,
                &[],
            )
            .await?;

        let mut state = crate::peers::store::PeersState {
            peers: crate::peers::store::PeerStore::load(peers),
            devices: crate::peers::store::DeviceStore::load(devices),
            access: crate::peers::store::AccessStore::load(grants),
            ..Default::default()
        };

        for row in limit_rows {
            state.limits.insert(
                row.get::<_, Uuid>("subscription_id"),
                (
                    row.get::<_, Option<i32>>("peers_per_device")
                        .map(i64::from),
                    row.get::<_, Option<i32>>("peers_per_subscription")
                        .map(i64::from),
                ),
            );
        }

        for row in network_rows {
            let env = Env::from_str(&row.get::<_, String>("env"))
                .unwrap_or_else(|_| Env::Custom(row.get::<_, String>("env")));
            state
                .env_networks
                .entry(env)
                .or_default()
                .push(row.get::<_, String>("subnet"));
        }

        for row in capacity_rows {
            let env = Env::from_str(&row.get::<_, String>("env"))
                .unwrap_or_else(|_| Env::Custom(row.get::<_, String>("env")));
            state.env_capacity.insert(
                env,
                (
                    row.get::<_, String>("plan"),
                    row.get::<_, Option<i32>>("max_subscriptions").map(i64::from),
                    row.get::<_, bool>("enabled"),
                ),
            );
        }

        Ok(state)
    }

    // ---------------------------------------------------------------- grants

    pub async fn grants(&self, subscription_id: Uuid) -> Result<Vec<AccessRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT id, env, plan, protos, sources, expires_at, traffic_limit
                FROM subscription_access
                WHERE subscription_id = $1 AND revoked_at IS NULL
                ORDER BY env
                "#,
                &[&subscription_id],
            )
            .await?;

        Ok(rows.iter().map(access_from_row).collect())
    }

    pub async fn grant(&self, subscription_id: Uuid, env: &Env) -> Result<Option<AccessRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT id, env, plan, protos, sources, expires_at, traffic_limit
                FROM subscription_access
                WHERE subscription_id = $1 AND env = $2 AND revoked_at IS NULL
                "#,
                &[&subscription_id, &env.to_string()],
            )
            .await?;

        Ok(rows.first().map(access_from_row))
    }

    /// Upsert by `(subscription, env)`: protocols and sources are unioned,
    /// the term takes the later value (NULL stays NULL = forever), and the
    /// traffic limit is replaced only when a non-NULL one is supplied.
    pub async fn upsert_grant(
        &self,
        subscription_id: Uuid,
        env: &Env,
        protos: &[Tag],
        source: &str,
        expires_at: Option<DateTime<Utc>>,
        traffic_limit: Option<i64>,
    ) -> Result<AccessRow> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let row = client
            .query_one(
                r#"
                INSERT INTO subscription_access
                    (subscription_id, env, protos, sources, expires_at, traffic_limit)
                VALUES ($1, $2, $3::proto[], ARRAY[$4]::text[], $5, $6)
                ON CONFLICT (subscription_id, env) WHERE revoked_at IS NULL DO UPDATE
                SET protos = (
                        SELECT ARRAY(SELECT DISTINCT unnest(subscription_access.protos || EXCLUDED.protos))
                    ),
                    sources = (
                        SELECT ARRAY(SELECT DISTINCT unnest(subscription_access.sources || EXCLUDED.sources))
                    ),
                    expires_at = CASE
                        WHEN subscription_access.expires_at IS NULL THEN EXCLUDED.expires_at
                        WHEN EXCLUDED.expires_at IS NULL THEN subscription_access.expires_at
                        ELSE GREATEST(subscription_access.expires_at, EXCLUDED.expires_at)
                    END,
                    traffic_limit = COALESCE(EXCLUDED.traffic_limit, subscription_access.traffic_limit),
                    updated_at = now()
                RETURNING id, env, plan, protos, sources, expires_at, traffic_limit
                "#,
                &[
                    &subscription_id,
                    &env.to_string(),
                    &protos.to_vec(),
                    &source,
                    &expires_at,
                    &traffic_limit,
                ],
            )
            .await?;

        Ok(access_from_row(&row))
    }

    /// Sticky assignment: the subscription keeps the shard it already has for
    /// this plan; a new one goes to the least-loaded shard among `shards`.
    ///
    /// `shards` is built by the caller from **live peer nodes** (plus the
    /// per-shard override), so an env whose machines are gone is never picked —
    /// assigning there would hand out a tunnel nobody serves.
    ///
    /// Runs under an advisory lock per plan: two concurrent payers must not
    /// read the same `used` count and land on the same shard twice.
    /// `Ok(None)` means "no live shard for this plan at all".
    #[allow(clippy::too_many_arguments)]
    pub async fn assign_grant(
        &self,
        subscription_id: Uuid,
        plan: &str,
        protos: &[Tag],
        source: &str,
        expires_at: Option<DateTime<Utc>>,
        traffic_limit: Option<i64>,
        shards: &[(String, i64)],
    ) -> Result<Option<(AccessRow, i64, i64, bool)>> {
        if shards.is_empty() {
            return Ok(None);
        }

        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let tx = client.transaction().await?;

        tx.execute(
            "SELECT pg_advisory_xact_lock(hashtext($1))",
            &[&format!("peers-plan:{}", plan)],
        )
        .await?;

        // Sticky first: an existing grant of this plan pins the shard.
        let existing = tx
            .query(
                r#"
                SELECT env
                FROM subscription_access
                WHERE subscription_id = $1 AND plan = $2 AND revoked_at IS NULL
                LIMIT 1
                "#,
                &[&subscription_id, &plan],
            )
            .await?;

        let env = match existing.first() {
            Some(row) => row.get::<_, String>("env"),
            None => {
                // Least loaded among the shards that actually have machines.
                let mut best: Option<(String, i64)> = None;
                for (candidate, limit) in shards {
                    let used: i64 = tx
                        .query_one(
                            r#"
                            SELECT count(*) AS used
                            FROM subscription_access
                            WHERE env = $1 AND plan = $2 AND revoked_at IS NULL
                            "#,
                            &[candidate, &plan],
                        )
                        .await?
                        .get("used");
                    let better = match &best {
                        None => true,
                        Some((best_env, best_used)) => {
                            used < *best_used || (used == *best_used && candidate < best_env)
                        }
                    };
                    if better {
                        best = Some((candidate.clone(), used));
                    }
                    let _ = limit;
                }
                match best {
                    Some((env, _)) => env,
                    None => return Ok(None),
                }
            }
        };

        let row = tx
            .query_one(
                r#"
                INSERT INTO subscription_access
                    (subscription_id, env, plan, protos, sources, expires_at, traffic_limit)
                VALUES ($1, $2, $3, $4::proto[], ARRAY[$5]::text[], $6, $7)
                ON CONFLICT (subscription_id, env) WHERE revoked_at IS NULL DO UPDATE
                SET protos = (
                        SELECT ARRAY(SELECT DISTINCT unnest(subscription_access.protos || EXCLUDED.protos))
                    ),
                    sources = (
                        SELECT ARRAY(SELECT DISTINCT unnest(subscription_access.sources || EXCLUDED.sources))
                    ),
                    plan = COALESCE(subscription_access.plan, EXCLUDED.plan),
                    expires_at = CASE
                        WHEN subscription_access.expires_at IS NULL THEN EXCLUDED.expires_at
                        WHEN EXCLUDED.expires_at IS NULL THEN subscription_access.expires_at
                        ELSE GREATEST(subscription_access.expires_at, EXCLUDED.expires_at)
                    END,
                    traffic_limit = COALESCE(EXCLUDED.traffic_limit, subscription_access.traffic_limit),
                    updated_at = now()
                RETURNING id, env, plan, protos, sources, expires_at, traffic_limit
                "#,
                &[
                    &subscription_id,
                    &env.to_string(),
                    &plan,
                    &protos.to_vec(),
                    &source,
                    &expires_at,
                    &traffic_limit,
                ],
            )
            .await?;

        let used: i64 = tx
            .query_one(
                r#"
                SELECT count(*) AS used
                FROM subscription_access
                WHERE env = $1 AND plan = $2 AND revoked_at IS NULL
                "#,
                &[&env, &plan],
            )
            .await?
            .get("used");

        let limit = shards
            .iter()
            .find(|(candidate, _)| candidate == &env)
            .map(|(_, limit)| *limit)
            .unwrap_or(0);
        let overflow = limit > 0 && used > limit;

        tx.commit().await?;

        Ok(Some((access_from_row(&row), used, limit, overflow)))
    }

    /// Registry upkeep on node registration: the shard and the pool exist and
    /// were seen now. Assignment never reads this table.
    pub async fn note_pool(
        &self,
        env: &Env,
        pool: &str,
        translation: Option<serde_json::Value>,
        node_count: i64,
    ) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .execute(
                r#"
                INSERT INTO pools (env, pool, pool_translation, node_count)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (env, pool) DO UPDATE
                SET pool_translation = COALESCE(EXCLUDED.pool_translation, pools.pool_translation),
                    last_seen = now(),
                    node_count = EXCLUDED.node_count
                "#,
                &[&env.to_string(), &pool, &translation, &node_count],
            )
            .await?;

        Ok(())
    }

    /// Capacity rows for a plan: `(env, limit)` where `limit` is the override,
    /// or `None` when it must be computed from the live node count.
    pub async fn plan_shards(&self, plan: &str) -> Result<Vec<(String, Option<i64>, bool)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT env, max_subscriptions, enabled
                FROM env_capacity
                WHERE plan = $1
                ORDER BY env
                "#,
                &[&plan],
            )
            .await?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>("env"),
                    r.get::<_, Option<i32>>("max_subscriptions").map(i64::from),
                    r.get::<_, bool>("enabled"),
                )
            })
            .collect())
    }

    /// Creates the capacity row on first sight of a shard (a node announced a
    /// peer interface for an env nobody registered yet).
    pub async fn ensure_shard_capacity(&self, env: &Env, plan: &str) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .execute(
                r#"
                INSERT INTO env_capacity (env, plan, max_subscriptions, enabled)
                VALUES ($1, $2, NULL, true)
                ON CONFLICT (env) DO NOTHING
                "#,
                &[&env.to_string(), &plan],
            )
            .await?;

        Ok(())
    }

    /// Close the quarantine row when the same key finally activates.
    pub async fn resolve_activation_failure(&self, key_id: Uuid) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .execute(
                r#"
                UPDATE activation_failures
                SET resolved_at = now()
                WHERE key_id = $1 AND resolved_at IS NULL
                "#,
                &[&key_id],
            )
            .await?;

        Ok(())
    }

    /// Record "paid but not served".
    pub async fn record_activation_failure(
        &self,
        key_id: Uuid,
        key_code: &str,
        product: &str,
        plan: &str,
        reason: &str,
        subscription_id: Option<Uuid>,
    ) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .execute(
                r#"
                INSERT INTO activation_failures
                    (key_id, key_code, product, plan, reason, subscription_id)
                VALUES ($1, $2, $3, $4, $5, $6)
                "#,
                &[
                    &key_id,
                    &key_code,
                    &product,
                    &plan,
                    &reason,
                    &subscription_id,
                ],
            )
            .await?;

        Ok(())
    }

    /// Per-shard load of a plan: `(env, used, limit)`.
    pub async fn capacity_for_plan(&self, plan: &str) -> Result<Vec<(String, i64, i64)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT c.env,
                       COALESCE((SELECT count(*) FROM subscription_access sa
                                  WHERE sa.env = c.env AND sa.plan = c.plan
                                    AND sa.revoked_at IS NULL), 0) AS used,
                       c.max_subscriptions AS lim
                FROM env_capacity c
                WHERE c.plan = $1
                ORDER BY c.env
                "#,
                &[&plan],
            )
            .await?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>("env"),
                    r.get::<_, i64>("used"),
                    r.get::<_, i32>("lim") as i64,
                )
            })
            .collect())
    }

    pub async fn revoke_grant(&self, subscription_id: Uuid, env: &Env) -> Result<bool> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let done = client
            .execute(
                r#"
                UPDATE subscription_access
                SET revoked_at = now(), updated_at = now()
                WHERE subscription_id = $1 AND env = $2 AND revoked_at IS NULL
                "#,
                &[&subscription_id, &env.to_string()],
            )
            .await?;

        Ok(done > 0)
    }

    // ------------------------------------------------------------ env blocks

    pub async fn env_blocks(&self, env: &Env) -> Result<Vec<IpAddrMask>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT subnet::text AS subnet
                FROM env_networks
                WHERE env = $1
                ORDER BY subnet
                "#,
                &[&env.to_string()],
            )
            .await?;

        Ok(rows
            .iter()
            .filter_map(|r| IpAddrMask::from_str(r.get::<_, String>("subnet").as_str()).ok())
            .collect())
    }

    pub async fn all_env_blocks(&self) -> Result<Vec<(String, IpAddrMask)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT env, subnet::text AS subnet
                FROM env_networks
                ORDER BY env, subnet
                "#,
                &[],
            )
            .await?;

        Ok(rows
            .iter()
            .filter_map(|r| {
                let env: String = r.get("env");
                let subnet: String = r.get("subnet");
                IpAddrMask::from_str(subnet.as_str())
                    .ok()
                    .map(|mask| (env, mask))
            })
            .collect())
    }

    /// Insert a block. The exclusion constraint refuses overlaps, so an empty
    /// result means "somebody (another env) already holds this range".
    pub async fn insert_env_block(&self, env: &Env, block: &IpAddrMask) -> Result<Inserted> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let inserted = client
            .execute(
                r#"
                INSERT INTO env_networks (env, subnet)
                VALUES ($1, $2::cidr)
                ON CONFLICT DO NOTHING
                "#,
                &[&env.to_string(), &block.to_string()],
            )
            .await?;

        Ok(if inserted > 0 {
            Inserted::Done
        } else {
            Inserted::Conflict
        })
    }

    /// Addresses currently held by live peers of one env.
    pub async fn used_addresses(&self, env: &Env) -> Result<Vec<Ipv4Addr>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT host(address) AS address
                FROM peers
                WHERE env = $1 AND revoked_at IS NULL
                "#,
                &[&env.to_string()],
            )
            .await?;

        Ok(rows
            .iter()
            .filter_map(|r| r.get::<_, String>("address").parse().ok())
            .collect())
    }

    // ---------------------------------------------------------------- limits

    pub async fn limits(&self, subscription_id: Uuid) -> Result<(Option<i64>, Option<i64>)> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT peers_per_device, peers_per_subscription
                FROM subscription_limits
                WHERE subscription_id = $1
                "#,
                &[&subscription_id],
            )
            .await?;

        Ok(match rows.first() {
            Some(row) => (
                row.get::<_, Option<i32>>("peers_per_device").map(i64::from),
                row.get::<_, Option<i32>>("peers_per_subscription")
                    .map(i64::from),
            ),
            None => (None, None),
        })
    }

    // --------------------------------------------------------------- devices

    pub async fn device_by_uid(
        &self,
        subscription_id: Uuid,
        device_uid: Uuid,
    ) -> Result<Option<DeviceRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT id, subscription_id, device_uid, device_name, public_key, platform,
                       support_tag, created_at
                FROM devices
                WHERE subscription_id = $1 AND device_uid = $2
                "#,
                &[&subscription_id, &device_uid],
            )
            .await?;

        Ok(rows.first().map(device_from_row))
    }

    pub async fn device_by_id(&self, id: Uuid) -> Result<Option<DeviceRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT id, subscription_id, device_uid, device_name, public_key, platform,
                       support_tag, created_at
                FROM devices
                WHERE id = $1
                "#,
                &[&id],
            )
            .await?;

        Ok(rows.first().map(device_from_row))
    }

    pub async fn device_by_support_tag(&self, tag: &str) -> Result<Option<DeviceRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT id, subscription_id, device_uid, device_name, public_key, platform,
                       support_tag, created_at
                FROM devices
                WHERE upper(support_tag) = upper($1)
                "#,
                &[&tag],
            )
            .await?;

        Ok(rows.first().map(device_from_row))
    }

    /// Devices of a subscription with their number of live tunnels.
    pub async fn devices(&self, subscription_id: Uuid) -> Result<Vec<(DeviceRow, i64)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT d.id, d.subscription_id, d.device_uid, d.device_name, d.public_key,
                       d.platform, d.support_tag, d.created_at,
                       (SELECT count(*) FROM peers p
                         WHERE p.device_id = d.id AND p.revoked_at IS NULL) AS live
                FROM devices d
                WHERE d.subscription_id = $1
                ORDER BY d.created_at
                "#,
                &[&subscription_id],
            )
            .await?;

        Ok(rows
            .iter()
            .map(|r| (device_from_row(r), r.get::<_, i64>("live")))
            .collect())
    }

    pub async fn insert_device(
        &self,
        subscription_id: Uuid,
        device_uid: Uuid,
        device_name: Option<&str>,
        public_key: &str,
        platform: Option<&str>,
        support_tag: &str,
    ) -> Result<Inserted> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let inserted = client
            .execute(
                r#"
                INSERT INTO devices
                    (subscription_id, device_uid, device_name, public_key, platform, support_tag)
                VALUES ($1, $2, $3, $4, $5, $6)
                ON CONFLICT DO NOTHING
                "#,
                &[
                    &subscription_id,
                    &device_uid,
                    &device_name,
                    &public_key,
                    &platform,
                    &support_tag,
                ],
            )
            .await?;

        Ok(if inserted > 0 {
            Inserted::Done
        } else {
            Inserted::Conflict
        })
    }

    pub async fn rename_device(&self, id: Uuid, name: Option<&str>) -> Result<bool> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let done = client
            .execute(
                "UPDATE devices SET device_name = $2 WHERE id = $1",
                &[&id, &name],
            )
            .await?;

        Ok(done > 0)
    }

    /// Deletes the device and cascades its peers. `peer_traffic` has no FK on
    /// purpose: the consumed quota must outlive the tunnel.
    pub async fn delete_device(&self, id: Uuid) -> Result<bool> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let done = client
            .execute("DELETE FROM devices WHERE id = $1", &[&id])
            .await?;

        Ok(done > 0)
    }

    /// Live tunnels of one device and of the whole subscription.
    pub async fn live_counts(&self, subscription_id: Uuid, device_id: Uuid) -> Result<(i64, i64)> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let row = client
            .query_one(
                r#"
                SELECT
                    (SELECT count(*) FROM peers
                      WHERE device_id = $2 AND revoked_at IS NULL) AS device_live,
                    (SELECT count(*) FROM peers
                      WHERE subscription_id = $1 AND revoked_at IS NULL) AS sub_live
                "#,
                &[&subscription_id, &device_id],
            )
            .await?;

        Ok((row.get("device_live"), row.get("sub_live")))
    }

    // ----------------------------------------------------------------- peers

    pub async fn peer_by_id(&self, id: Uuid) -> Result<Option<PeerRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                &format!("SELECT {} FROM peers WHERE id = $1", PEER_COLUMNS),
                &[&id],
            )
            .await?;

        Ok(rows.first().map(peer_from_row))
    }

    pub async fn live_peer_by_key(&self, env: &Env, public_key: &str) -> Result<Option<PeerRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                &format!(
                    "SELECT {} FROM peers WHERE env = $1 AND public_key = $2 AND revoked_at IS NULL",
                    PEER_COLUMNS
                ),
                &[&env.to_string(), &public_key],
            )
            .await?;

        Ok(rows.first().map(peer_from_row))
    }

    /// Live tunnels plus those revoked in the last `remember_days`, so the app
    /// can tell "the server dropped me" from "the subscription ended".
    pub async fn peers_of(
        &self,
        subscription_id: Uuid,
        device_id: Option<Uuid>,
        remember_days: i64,
    ) -> Result<Vec<PeerRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM peers
                    WHERE subscription_id = $1
                      AND ($2::uuid IS NULL OR device_id = $2)
                      AND (revoked_at IS NULL
                           OR revoked_at > now() - ($3 || ' days')::interval)
                    ORDER BY created_at DESC
                    "#,
                    PEER_COLUMNS
                ),
                &[&subscription_id, &device_id, &remember_days.to_string()],
            )
            .await?;

        Ok(rows.iter().map(peer_from_row).collect())
    }

    /// Every live tunnel, for the traffic persister.
    pub async fn all_live_peers(&self) -> Result<Vec<PeerRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                &format!(
                    "SELECT {} FROM peers WHERE revoked_at IS NULL",
                    PEER_COLUMNS
                ),
                &[],
            )
            .await?;

        Ok(rows.iter().map(peer_from_row).collect())
    }

    /// Accumulate peer traffic into one day bucket. Daily rows are the
    /// canonical lifetime total; monthly rows are not written for peers.
    pub async fn upsert_peer_traffic(
        &self,
        peer_id: Uuid,
        subscription_id: Uuid,
        env: &Env,
        bucket: DateTime<Utc>,
        uplink: i64,
        downlink: i64,
    ) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .execute(
                r#"
                INSERT INTO peer_traffic
                    (peer_id, subscription_id, env, period, bucket, uplink_bytes, downlink_bytes)
                VALUES ($1, $2, $3, 'day', $4, $5, $6)
                ON CONFLICT (peer_id, period, bucket) DO UPDATE
                SET uplink_bytes = peer_traffic.uplink_bytes + EXCLUDED.uplink_bytes,
                    downlink_bytes = peer_traffic.downlink_bytes + EXCLUDED.downlink_bytes,
                    updated_at = now()
                "#,
                &[
                    &peer_id,
                    &subscription_id,
                    &env.to_string(),
                    &bucket,
                    &uplink,
                    &downlink,
                ],
            )
            .await?;

        Ok(())
    }

    /// Every live tunnel of an env — the authoritative set a node receives on
    /// its `peers-init` topic.
    pub async fn live_peers_of_env(&self, env: &Env) -> Result<Vec<PeerRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                &format!(
                    "SELECT {} FROM peers WHERE env = $1 AND revoked_at IS NULL ORDER BY created_at",
                    PEER_COLUMNS
                ),
                &[&env.to_string()],
            )
            .await?;

        Ok(rows.iter().map(peer_from_row).collect())
    }

    /// Insert a tunnel and return the stored row, so the caller can put the
    /// same row into memory and into the bus without a second SELECT.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_peer(
        &self,
        device_id: Uuid,
        subscription_id: Uuid,
        env: &Env,
        proto: Tag,
        public_key: &str,
        address: Ipv4Addr,
        label: Option<&str>,
    ) -> Result<Option<PeerRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let host = format!("{}/32", address);
        let row = client
            .query_opt(
                &format!(
                    r#"
                    INSERT INTO peers
                        (device_id, subscription_id, env, proto, public_key, address, label)
                    VALUES ($1, $2, $3, $4, $5, $6::cidr, $7)
                    ON CONFLICT DO NOTHING
                    RETURNING {PEER_COLUMNS}
                    "#
                ),
                &[
                    &device_id,
                    &subscription_id,
                    &env.to_string(),
                    &proto,
                    &public_key,
                    &host,
                    &label,
                ],
            )
            .await?;

        Ok(row.as_ref().map(peer_from_row))
    }

    pub async fn revoke_peer(&self, id: Uuid, reason: &str) -> Result<bool> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let done = client
            .execute(
                r#"
                UPDATE peers
                SET revoked_at = now(), revoke_reason = $2
                WHERE id = $1 AND revoked_at IS NULL
                "#,
                &[&id, &reason],
            )
            .await?;

        Ok(done > 0)
    }

    /// Revokes every live peer of a subscription in one env (expired grant,
    /// exhausted quota) and returns how many were taken down.
    pub async fn revoke_env_peers(
        &self,
        subscription_id: Uuid,
        env: &Env,
        reason: &str,
    ) -> Result<i64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let done = client
            .execute(
                r#"
                UPDATE peers
                SET revoked_at = now(), revoke_reason = $3
                WHERE subscription_id = $1 AND env = $2 AND revoked_at IS NULL
                "#,
                &[&subscription_id, &env.to_string(), &reason],
            )
            .await?;

        Ok(done as i64)
    }

    // --------------------------------------------------------------- traffic

    /// Lifetime traffic of the subscription in one env (daily rows only;
    /// monthly rows are a separate aggregate and would double count).
    pub async fn used_bytes(&self, subscription_id: Uuid, env: &Env) -> Result<i64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let row = client
            .query_one(
                r#"
                SELECT COALESCE(SUM(uplink_bytes + downlink_bytes), 0)::BIGINT AS used
                FROM subscription_traffic
                WHERE subscription_id = $1 AND env = $2 AND period = 'day'
                "#,
                &[&subscription_id, &env.to_string()],
            )
            .await?;

        Ok(row.get("used"))
    }
}

const PEER_COLUMNS: &str = "id, device_id, subscription_id, env, proto, public_key, \
                            host(address) AS address, label, created_at, revoked_at, revoke_reason";

fn access_from_row(row: &tokio_postgres::Row) -> AccessRow {
    let env: String = row.get("env");
    AccessRow {
        id: row.get("id"),
        env: Env::from_str(env.as_str()).unwrap_or(Env::Custom(env)),
        plan: row.get("plan"),
        protos: row.get("protos"),
        sources: row.get("sources"),
        expires_at: row.get("expires_at"),
        traffic_limit: row.get("traffic_limit"),
    }
}

fn device_from_row(row: &tokio_postgres::Row) -> DeviceRow {
    DeviceRow {
        id: row.get("id"),
        subscription_id: row.get("subscription_id"),
        device_uid: row.get("device_uid"),
        device_name: row.get("device_name"),
        public_key: row.get("public_key"),
        platform: row.get("platform"),
        support_tag: row.get("support_tag"),
        created_at: row.get("created_at"),
    }
}

fn peer_from_row(row: &tokio_postgres::Row) -> PeerRow {
    let env: String = row.get("env");
    PeerRow {
        id: row.get("id"),
        device_id: row.get("device_id"),
        subscription_id: row.get("subscription_id"),
        env: Env::from_str(env.as_str()).unwrap_or(Env::Custom(env)),
        proto: row.get("proto"),
        public_key: row.get("public_key"),
        address: row.get("address"),
        label: row.get("label"),
        created_at: row.get("created_at"),
        revoked_at: row.get("revoked_at"),
        revoke_reason: row.get("revoke_reason"),
    }
}
