//! Peer storage: grants (`subscription_access`), devices, tunnels (`peers`),
//! env blocks (`env_networks`) and the per-subscription limit overrides.
//!
//! Raw `tokio_postgres` queries, like the rest of `postgres/`. Every write
//! that can lose a race uses `ON CONFLICT DO NOTHING` and reports "nothing
//! inserted" instead of an error: the caller decides whether that means
//! "take the next address" or "this key is already live".

use std::collections::{HashMap, HashSet};
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

/// What a renewal did to one grant row: the row to write into memory, whether
/// it came back from a revocation, and why it had been revoked.
#[derive(Clone, Debug)]
pub struct RenewedGrant {
    pub row: AccessRow,
    pub revived: bool,
    pub revoked_reason: Option<String>,
}

/// What a plan purchase did (`POST /subscription/{id}/plan`).
#[derive(Clone, Debug)]
pub struct PlanFulfilment {
    /// Grant the purchase wrote or extended; `None` when no shard could take
    /// the plan (the caller answers 503, the payment is not turned into rights).
    pub row: Option<AccessRow>,
    /// The payment reference was already recorded: this call changed nothing.
    pub duplicate: bool,
    /// Live grants on the shard and the shard's limit (`0` = no declared cap).
    pub used: i64,
    pub limit: i64,
    pub overflow: bool,
}

/// One row of `shard_overview`: a declared shard with its live numbers.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ShardRow {
    pub shard: String,
    pub title: Option<String>,
    pub plan: String,
    /// Shard class (`core | premium | personal | legacy`). Derived from the
    /// plan on write; never from the id, which is opaque.
    pub kind: String,
    pub enabled: bool,
    /// Pools the shard's nodes declare — the cards a client sees, not a count.
    pub pools: Vec<String>,
    pub nodes: i64,
    pub peers_live: i64,
    pub address_hosts: i64,
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
    /// Set when the device's default key was replaced (the app shows it).
    pub rotated_at: Option<DateTime<Utc>>,
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
    /// Card the tunnel was created for (`<shard>:<pool>:<proto>` → the middle
    /// part). `None` for rows written before the column existed: their pool
    /// cannot be recovered, so the app falls back to env + proto.
    pub pool: Option<String>,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
}

/// Watermark of the peer traffic persister: how far the peer's traffic has been
/// booked and the totals already written. The peer-side counterpart of
/// `ConnWatermark` (`connections.uplink/downlink/last_traffic_persist_at`).
#[derive(Clone, Debug)]
pub struct PeerWatermark {
    pub peer_id: Uuid,
    pub subscription_id: Uuid,
    pub env: Env,
    pub uplink: i64,
    pub downlink: i64,
    pub last_persist_at: DateTime<Utc>,
}

/// What an insert actually did. `Conflict` means the unique index refused it;
/// the caller retries with the next address or reports a taken key/label.
#[derive(Clone, Debug)]
pub enum Inserted {
    Done,
    Conflict,
}

/// One live peer as the cleanup job sees it: its last booked traffic and the
/// facts about the subscription and grant that own it. The decision itself is
/// `crate::peers::reap_reason` — pure and tested without a database.
#[derive(Clone, Debug)]
pub struct PeerCleanupRow {
    pub peer: PeerRow,
    /// Last row written to `peer_traffic` for this peer; `None` when it never
    /// moved a byte (the idle clock then starts at `peer.created_at`).
    pub last_traffic_at: Option<DateTime<Utc>>,
    /// `subscriptions.is_deleted`.
    pub sub_deleted: bool,
    /// `subscriptions.expires_at` is set and already over.
    pub sub_expired: bool,
    /// The env is the subscription's own (self-hosted node, premium slot).
    pub env_owned: bool,
    /// A grant of this (subscription, env) is live.
    pub grant_live: bool,
    /// Any grant row (live or revoked) exists for this (subscription, env).
    pub grant_seen: bool,
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
                       support_tag, created_at, rotated_at
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
            .query(
                "SELECT env, pool, subnet::text AS subnet FROM env_networks ORDER BY env, pool, subnet",
                &[],
            )
            .await?;

        let capacity_rows = client
            .query(
                r#"
                SELECT env, plan, max_subscriptions, enabled,
                       to_jsonb(c) ->> 'kind'  AS kind,
                       to_jsonb(c) ->> 'title' AS title
                FROM env_capacity c
                "#,
                &[],
            )
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
            let env_name = row.get::<_, String>("env");
            let env = Env::from_str(&env_name).unwrap_or_else(|_| Env::Custom(env_name));
            let subnet = row.get::<_, String>("subnet");
            match row.get::<_, Option<String>>("pool") {
                // Pool-scoped block: the allocator uses it instead of the
                // env-wide one, so an address names its pool.
                Some(pool) if !pool.is_empty() => state
                    .pool_networks
                    .entry((env, pool))
                    .or_default()
                    .push(subnet),
                _ => state.env_networks.entry(env).or_default().push(subnet),
            }
        }

        for row in capacity_rows {
            let env = Env::from_str(&row.get::<_, String>("env"))
                .unwrap_or_else(|_| Env::Custom(row.get::<_, String>("env")));
            let kind = row
                .get::<_, Option<String>>("kind")
                .as_deref()
                .and_then(crate::peers::ShardKind::parse)
                // Rows written before `kind` existed carry `legacy`, which is
                // not a kind: falling back to Core made every personal shard
                // look like a region and refused private-node registration
                // ("invalid personal scope"). Derive it from the plan, then
                // from the env name — both are what the row really means.
                .or_else(|| {
                    let plan = row.get::<_, Option<String>>("plan").unwrap_or_default();
                    crate::peers::ShardKind::for_plan(&plan)
                })
                .or_else(|| {
                    let env = row.get::<_, String>("env");
                    if env.starts_with("custompersonal")
                        || env.starts_with("personal")
                        || env.starts_with("sf")
                    {
                        Some(crate::peers::ShardKind::Selfhosted)
                    } else if env
                        .strip_prefix("custom")
                        .map(|rest| rest.len() >= 8 && rest.chars().all(|c| c.is_ascii_hexdigit()))
                        .unwrap_or(false)
                    {
                        // A reseller shard: `custom<hex>`. Anything else
                        // custom-shaped stays a legacy region.
                        Some(crate::peers::ShardKind::Resell)
                    } else if env.starts_with("premium") {
                        Some(crate::peers::ShardKind::Premium)
                    } else {
                        Some(crate::peers::ShardKind::Core)
                    }
                })
                .unwrap_or_default();
            state.env_capacity.insert(
                env,
                crate::peers::store::ShardCapacity {
                    plan: row.get("plan"),
                    max_subscriptions: row.get::<_, Option<i32>>("max_subscriptions").map(i64::from),
                    enabled: row.get("enabled"),
                    title: row.get("title"),
                    kind,
                },
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
        announced: &HashMap<String, HashSet<Tag>>,
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
        let Some(env) = pick_shard(&tx, subscription_id, plan, shards).await? else {
            return Ok(None);
        };

        // A shard serves exactly what its nodes announce; the grant must not
        // name anything else (see `filter_protos_for_env`).
        let protos: Vec<Tag> = filter_protos_for_env(protos, &env.to_string(), announced);

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

    /// Fulfil a plan purchase: write (or extend) the grant a cabinet purchase
    /// paid for, exactly once.
    ///
    /// `payment_marker` is the recorded payment reference (`pay:<ref>` in
    /// `sources`). The whole call runs in one transaction under an advisory
    /// lock keyed by the reference, so a webhook delivered twice — or delivered
    /// twice at the same instant — extends the term, the quota and the shard
    /// count only once and answers `duplicate: true` the second time. No new
    /// table: `sources` already answers «what bought this row».
    ///
    /// `days` and `add_traffic_bytes` are what was bought; the arithmetic
    /// (extension, quota top-up) lives in `plans::purchase_terms` and is applied
    /// to the row read here, under the same lock, so two concurrent purchases
    /// cannot both start from the same old value.
    #[allow(clippy::too_many_arguments)]
    pub async fn fulfil_plan_grant(
        &self,
        subscription_id: Uuid,
        plan: &str,
        protos: &[Tag],
        source: &str,
        payment_marker: &str,
        days: i64,
        add_traffic_bytes: i64,
        shards: &[(String, i64)],
        announced: &HashMap<String, HashSet<Tag>>,
    ) -> Result<PlanFulfilment> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let tx = client.transaction().await?;

        tx.execute(
            "SELECT pg_advisory_xact_lock(hashtext($1))",
            &[&format!("plan-buy:{}:{}", subscription_id, payment_marker)],
        )
        .await?;

        // The same payment already bought something: return what it bought and
        // change nothing. Any row of the plan counts, revoked or not — a
        // duplicate webhook must not revive a grant support has taken away.
        let already = tx
            .query_opt(
                r#"
                SELECT id, env, plan, protos, sources, expires_at, traffic_limit
                FROM subscription_access
                WHERE subscription_id = $1 AND plan = $2 AND $3 = ANY(sources)
                ORDER BY (revoked_at IS NULL) DESC, created_at DESC
                LIMIT 1
                "#,
                &[&subscription_id, &plan, &payment_marker],
            )
            .await?;

        if let Some(row) = already {
            let row = access_from_row(&row);
            let (used, limit, overflow) =
                shard_load(&tx, &row.env.to_string(), plan, shards).await?;
            tx.commit().await?;
            return Ok(PlanFulfilment {
                row: Some(row),
                duplicate: true,
                used,
                limit,
                overflow,
            });
        }

        let Some(env) = pick_shard(&tx, subscription_id, plan, shards).await? else {
            // No shard can take this plan: the caller answers 503 and the money
            // is not turned into rights nobody serves.
            tx.rollback().await.ok();
            return Ok(PlanFulfilment {
                row: None,
                duplicate: false,
                used: 0,
                limit: 0,
                overflow: false,
            });
        };

        // Same filter as `assign_grant`: the grant names only what the chosen
        // shard actually serves.
        let protos: Vec<Tag> = filter_protos_for_env(protos, &env.to_string(), announced);

        // The row this purchase extends, locked: the term and the quota are
        // computed from it below.
        let existing = tx
            .query_opt(
                r#"
                SELECT id, env, plan, protos, sources, expires_at, traffic_limit
                FROM subscription_access
                WHERE subscription_id = $1 AND env = $2 AND revoked_at IS NULL
                FOR UPDATE
                "#,
                &[&subscription_id, &env],
            )
            .await?;
        let existing_row = existing.as_ref().map(access_from_row);

        let terms = crate::plans::purchase_terms(
            plan,
            days,
            (add_traffic_bytes > 0).then_some(add_traffic_bytes),
            existing_row
                .as_ref()
                .map(|row| (row.expires_at, row.traffic_limit)),
            Utc::now(),
        );

        // The payment reference rides in `sources`: it is both the record of
        // what paid for this row and the key the duplicate check above reads.
        let sources = crate::plans::purchase_sources(source, payment_marker);

        let row = tx
            .query_one(
                r#"
                INSERT INTO subscription_access
                    (subscription_id, env, plan, protos, sources, expires_at, traffic_limit)
                VALUES ($1, $2, $3, $4::proto[], $5, $6, $7)
                ON CONFLICT (subscription_id, env) WHERE revoked_at IS NULL DO UPDATE
                SET protos = (
                        SELECT ARRAY(SELECT DISTINCT unnest(subscription_access.protos || EXCLUDED.protos))
                    ),
                    sources = (
                        SELECT ARRAY(SELECT DISTINCT unnest(subscription_access.sources || EXCLUDED.sources))
                    ),
                    plan = COALESCE(subscription_access.plan, EXCLUDED.plan),
                    expires_at = EXCLUDED.expires_at,
                    traffic_limit = EXCLUDED.traffic_limit,
                    updated_at = now()
                RETURNING id, env, plan, protos, sources, expires_at, traffic_limit
                "#,
                &[
                    &subscription_id,
                    &env,
                    &plan,
                    &protos.to_vec(),
                    &sources,
                    &terms.expires_at,
                    &terms.traffic_limit,
                ],
            )
            .await?;

        let (used, limit, overflow) = shard_load(&tx, &env, plan, shards).await?;
        tx.commit().await?;

        Ok(PlanFulfilment {
            row: Some(access_from_row(&row)),
            duplicate: false,
            used,
            limit,
            overflow,
        })
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

        // jsonb has no `ToSql` without tokio-postgres' serde_json feature, so
        // the translation travels as text and is cast here; `node_count` is
        // `integer` while the caller speaks i64. Both mismatches used to kill
        // the insert, and the caller swallowed the error — the registry stayed
        // empty for every shard.
        // `$3::text::jsonb` keeps the parameter type `text` (a bare `$3::jsonb`
        // makes it jsonb, and a String then fails to serialize into it);
        // `node_count` is `integer`, the caller speaks i64.
        let translation = translation.map(|value| value.to_string());
        let node_count = node_count as i32;

        client
            .execute(
                r#"
                INSERT INTO pools (env, pool, pool_translation, node_count)
                VALUES ($1, $2, $3::text::jsonb, $4)
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

    /// Admin: declare a shard, or update its policy. The plan, the human title
    /// and the kind live here and never in a node config: a shard id is opaque,
    /// and policy is the api's decision, not a node's.
    ///
    /// `None` leaves the stored value alone (a title, a hand-set cap), so the
    /// same call can be used to flip `enabled` without restating the rest.
    pub async fn upsert_shard(
        &self,
        env: &Env,
        plan: &str,
        title: Option<&str>,
        kind: &str,
        max_subscriptions: Option<i32>,
        enabled: bool,
    ) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .execute(
                r#"
                INSERT INTO env_capacity (env, plan, max_subscriptions, enabled, title, kind)
                VALUES ($1, $2, $3, $4, $5, $6)
                ON CONFLICT (env) DO UPDATE
                SET plan = EXCLUDED.plan,
                    max_subscriptions = COALESCE(EXCLUDED.max_subscriptions, env_capacity.max_subscriptions),
                    enabled = EXCLUDED.enabled,
                    title = COALESCE(EXCLUDED.title, env_capacity.title),
                    kind = EXCLUDED.kind
                "#,
                &[
                    &env.to_string(),
                    &plan,
                    &max_subscriptions,
                    &enabled,
                    &title,
                    &kind,
                ],
            )
            .await?;

        Ok(())
    }

    /// Admin: forget a shard entirely — its policy row and its address blocks.
    ///
    /// Callers must check that nothing lives there first: dropping the blocks of
    /// a shard that still has peers leaves them unaddressable.
    pub async fn delete_shard(&self, env: &Env) -> Result<bool> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .execute(
                "DELETE FROM env_networks WHERE env = $1",
                &[&env.to_string()],
            )
            .await?;
        let policy = client
            .execute(
                "DELETE FROM env_capacity WHERE env = $1",
                &[&env.to_string()],
            )
            .await?;

        Ok(policy > 0)
    }

    /// Admin: does anything still depend on the shard?
    pub async fn shard_usage(&self, env: &Env) -> Result<(i64, i64)> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_one(
                r#"
                SELECT (SELECT COUNT(*)::int8 FROM nodes WHERE env = $1) AS nodes,
                       (SELECT COUNT(*)::int8 FROM peers WHERE env = $1 AND revoked_at IS NULL) AS peers
                "#,
                &[&env.to_string()],
            )
            .await?;
        Ok((row.get("nodes"), row.get("peers")))
    }

    /// Admin: every declared shard with its live numbers, straight from the
    /// `shard_overview` view (migration 0021).
    pub async fn list_shards(&self) -> Result<Vec<ShardRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT shard, title, plan, kind, enabled,
                       -- `pools` is text[] (the card names of the shard), the
                       -- rest are counts. Casting the array to int8 made this
                       -- query, and with it `GET /admin/api/shards`, fail.
                       pools, nodes::int8 AS nodes,
                       peers_live::int8 AS peers_live,
                       address_hosts::int8 AS address_hosts
                  FROM shard_overview
                 ORDER BY shard
                "#,
                &[],
            )
            .await?;

        Ok(rows
            .iter()
            .map(|row| ShardRow {
                shard: row.get("shard"),
                title: row.get("title"),
                plan: row.get("plan"),
                kind: row.get("kind"),
                enabled: row.get("enabled"),
                pools: row.get("pools"),
                nodes: row.get("nodes"),
                peers_live: row.get("peers_live"),
                address_hosts: row.get("address_hosts"),
            })
            .collect())
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

    /// Human titles of the shards (`env → title`). Read through `to_jsonb` so
    /// an api that starts before migration 0021 ran still serves /status
    /// instead of failing the whole query on a missing column.
    pub async fn shard_titles(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT env, to_jsonb(c) ->> 'title' AS title
                FROM env_capacity c
                WHERE to_jsonb(c) ->> 'title' IS NOT NULL
                "#,
                &[],
            )
            .await?;

        Ok(rows
            .iter()
            .filter_map(|r| {
                let env: String = r.get("env");
                let title: Option<String> = r.get("title");
                title.map(|t| (env, t))
            })
            .collect())
    }

    /// Per-shard load of a plan: `(env, used, limit)`.
    pub async fn capacity_for_plan(&self, plan: &str) -> Result<Vec<(String, i64, Option<i64>)>> {
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
                    r.get::<_, Option<i32>>("lim").map(i64::from),
                )
            })
            .collect())
    }

    /// Renewal: a grant keeps its own `expires_at` (each shard right has its own
    /// term), so extending the subscription must move the grants too —
    /// otherwise a paying user loses their tunnels the day the old term ends.
    ///
    /// A grant holds its own term, so `expire_grants` closes it with reason
    /// `expired` (or `quota`) the moment that term runs out. Without reviving
    /// those rows here, paying for another month would move the subscription
    /// and leave the user without a tunnel forever — hence one row here is
    /// either extended (live) or revived (revoked by the clock or the meter),
    /// never re-assigned: the env, pool and protos of the shard stay the same.
    ///
    /// Explicit revocations (`user`, `subscription_ended`) and rows of a
    /// subscription that never had a grant are left alone. Rows are locked
    /// `FOR UPDATE` so a concurrent expiry tick cannot revoke between the read
    /// and the write.
    pub async fn renew_grants(
        &self,
        subscription_id: Uuid,
        expires_at: chrono::DateTime<Utc>,
    ) -> Result<Vec<RenewedGrant>> {
        // A "renewal" into the past is not a renewal: reviving a grant with an
        // already-over term would just be revoked again by the next tick.
        if expires_at <= Utc::now() {
            return Ok(Vec::new());
        }

        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let tx = client.transaction().await?;

        let rows = tx
            .query(
                r#"
                SELECT id, env, plan, protos, sources, expires_at, traffic_limit,
                       revoked_at, revoked_reason
                  FROM subscription_access
                 WHERE subscription_id = $1
                 ORDER BY env
                 FOR UPDATE
                "#,
                &[&subscription_id],
            )
            .await?;

        // A live row for an env wins over a revoked one: reviving the second
        // would break the `WHERE revoked_at IS NULL` unique index.
        let live_envs: Vec<String> = rows
            .iter()
            .filter(|row| row.get::<_, Option<DateTime<Utc>>>("revoked_at").is_none())
            .map(|row| row.get::<_, String>("env"))
            .collect();

        let mut renewed = Vec::new();
        for row in &rows {
            let id: Uuid = row.get("id");
            let env_raw: String = row.get("env");
            let revoked_reason: Option<String> = row.get("revoked_reason");
            let was_live = row.get::<_, Option<DateTime<Utc>>>("revoked_at").is_none();

            let access = access_from_row(row);
            let state = if was_live {
                crate::peers::GrantState::live(crate::peers::Grant {
                    env: access.env.clone(),
                    protos: access.protos.clone(),
                    sources: access.sources.clone(),
                    expires_at: access.expires_at,
                    traffic_limit: access.traffic_limit,
                })
            } else {
                crate::peers::GrantState::revoked(
                    crate::peers::Grant {
                        env: access.env.clone(),
                        protos: access.protos.clone(),
                        sources: access.sources.clone(),
                        expires_at: access.expires_at,
                        traffic_limit: access.traffic_limit,
                    },
                    revoked_reason.as_deref(),
                )
            };

            if !was_live && live_envs.contains(&env_raw) {
                continue;
            }

            // Traffic is summed lifetime, like `over_quota_grants` does: a
            // revived quota row must not be spent already, or the next tick
            // would revoke it again.
            let used = if !was_live && revoked_reason.as_deref() == Some(crate::peers::REVOKE_QUOTA)
            {
                tx.query_one(
                    r#"
                    SELECT COALESCE(SUM(uplink_bytes + downlink_bytes), 0)::BIGINT AS used
                      FROM subscription_traffic
                     WHERE subscription_id = $1 AND env = $2 AND period = 'day'
                    "#,
                    &[&subscription_id, &env_raw],
                )
                .await?
                .get("used")
            } else {
                0
            };

            let Some(next) = state.renewed(expires_at, used) else {
                continue;
            };

            tx.execute(
                r#"
                UPDATE subscription_access
                   SET expires_at = $2, revoked_at = NULL, revoked_reason = NULL,
                       updated_at = now()
                 WHERE id = $1
                "#,
                &[&id, &next.expires_at],
            )
            .await?;

            renewed.push(RenewedGrant {
                row: AccessRow {
                    expires_at: next.expires_at,
                    ..access
                },
                revived: !was_live,
                revoked_reason,
            });
        }

        tx.commit().await?;

        Ok(renewed)
    }

    pub async fn revoke_grant(
        &self,
        subscription_id: Uuid,
        env: &Env,
        reason: &str,
    ) -> Result<bool> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let done = client
            .execute(
                r#"
                UPDATE subscription_access
                SET revoked_at = now(), revoked_reason = $3, updated_at = now()
                WHERE subscription_id = $1 AND env = $2 AND revoked_at IS NULL
                "#,
                &[&subscription_id, &env.to_string(), &reason],
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

    /// Insert a block for `(env, pool)`. `pool = None` stores the env-wide
    /// block (the shard's own /16). The unique index makes a re-run a no-op.
    pub async fn upsert_network(
        &self,
        env: &Env,
        pool: Option<&str>,
        block: &IpAddrMask,
    ) -> Result<Inserted> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let inserted = client
            .execute(
                r#"
                INSERT INTO env_networks (env, pool, subnet)
                VALUES ($1, $2, $3::text::cidr)
                ON CONFLICT DO NOTHING
                "#,
                &[&env.to_string(), &pool.map(|p| p.to_string()), &block.to_string()],
            )
            .await?;

        Ok(if inserted > 0 {
            Inserted::Done
        } else {
            Inserted::Conflict
        })
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
                VALUES ($1, $2::text::cidr)
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

    /// Carve a fresh block for `env` out of `pool` and store it.
    ///
    /// Shards whose number is unknown (premium slots, customers' machines)
    /// cannot be listed in the api config, so their address space is allocated on
    /// first use. Returns the block, or `None` when the pool is exhausted.
    pub async fn allocate_env_block(
        &self,
        env: &Env,
        pool: &IpAddrMask,
        prefix: u8,
    ) -> Result<Option<IpAddrMask>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query("SELECT subnet::text AS subnet FROM env_networks", &[])
            .await?;
        let mut taken: Vec<IpAddrMask> = rows
            .iter()
            .filter_map(|r| IpAddrMask::from_str(r.get::<_, String>("subnet").as_str()).ok())
            .collect();

        // The exclusion constraint is the real judge: a block another api
        // instance grabbed between our read and our insert bounces, and we take
        // the next one.
        for _ in 0..64 {
            let Some(candidate) = crate::peers::alloc::next_block(pool, &taken, prefix) else {
                return Ok(None);
            };
            let inserted = client
                .execute(
                    r#"
                    INSERT INTO env_networks (env, subnet)
                    VALUES ($1, $2::text::cidr)
                    ON CONFLICT DO NOTHING
                    "#,
                    &[&env.to_string(), &candidate.to_string()],
                )
                .await?;
            if inserted > 0 {
                return Ok(Some(candidate));
            }
            taken.push(candidate);
        }

        Ok(None)
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
                       support_tag, created_at, rotated_at
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
                       support_tag, created_at, rotated_at
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
                       support_tag, created_at, rotated_at
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
                       d.platform, d.support_tag, d.created_at, d.rotated_at,
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

    /// Idempotent DDL of the peers layer.
    ///
    /// `0025`/`0030` are applied before the binary on a normal deploy, but a
    /// rollback puts an older tree under a newer database and vice versa; the
    /// api must not fail every peer query because one column is missing.
    pub async fn ensure_schema(&self) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let row = client
            .query_one("SELECT to_regclass('peers') IS NOT NULL AS present", &[])
            .await?;
        if !row.get::<_, bool>("present") {
            return Ok(());
        }

        client
            .batch_execute(
                "ALTER TABLE peers ADD COLUMN IF NOT EXISTS pool text;
                 CREATE INDEX IF NOT EXISTS peers_pool_live
                     ON peers (env, pool) WHERE revoked_at IS NULL;
                 -- One key per (env, pool) since `POST /v1/peers/prepare`: a
                 -- device holds one tunnel in every pool of the shard.
                 CREATE UNIQUE INDEX IF NOT EXISTS peers_live_pool_key
                     ON peers (env, COALESCE(pool, ''), public_key) WHERE revoked_at IS NULL;
                 DROP INDEX IF EXISTS peers_live_key;
                 ALTER TABLE subscription_access
                     ADD COLUMN IF NOT EXISTS revoked_reason text;
                 -- Migration marker (0030): a fresh subscription is on the v1
                 -- plane until its first live peer moves it to v2/dual.
                 ALTER TABLE subscriptions
                     ADD COLUMN IF NOT EXISTS plane text NOT NULL DEFAULT 'v1',
                     ADD COLUMN IF NOT EXISTS v2_since timestamptz;
                  -- Self-hosted shard numbers (`sf0001`). A sequence, not
                  -- max()+1: two accounts created at the same time must not get
                  -- the same shard, and a revoked one must not be handed out
                  -- again while its nodes still exist.
                  CREATE SEQUENCE IF NOT EXISTS selfhosted_shard_seq START WITH 1;",
            )
            .await?;

        Ok(())
    }

    /// Next free self-hosted shard name (`sf0001`, `sf0002`, …).
    ///
    /// Numbers are never reused: a shard whose account was deleted keeps its
    /// name in `env_capacity`, `env_networks` and possibly on a node that is
    /// still running, so handing the number to a new account would put two
    /// owners in one env.
    pub async fn next_selfhosted_shard(&self) -> Result<fcore::Env> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let row = client
            .query_one("SELECT nextval('selfhosted_shard_seq') AS n", &[])
            .await?;
        let number: i64 = row.get("n");
        Ok(fcore::Env::selfhosted(number as u64))
    }

    /// Deletes devices that are only history: no live tunnel and nothing
    /// created for `days`.
    ///
    /// A device row is what makes an installation known to the api (`device_uid`
    /// survives a reinstall because the app keeps it in the Keychain), so
    /// without this the device cap fills up with phones that no longer exist and
    /// support has to delete them by hand. Only devices without a live peer are
    /// touched — an active installation always has one — and its peers are
    /// already revoked, so nothing has to be pushed to a node.
    pub async fn reap_stale_devices(&self, days: i64, limit: i64) -> Result<Vec<DeviceRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                DELETE FROM devices d
                WHERE d.id IN (
                    SELECT c.id
                      FROM devices c
                     WHERE NOT EXISTS (
                               SELECT 1 FROM peers p
                                WHERE p.device_id = c.id AND p.revoked_at IS NULL)
                       AND GREATEST(
                               c.created_at,
                               COALESCE(c.rotated_at, c.created_at),
                               COALESCE((SELECT max(GREATEST(
                                                       p.created_at,
                                                       COALESCE(p.revoked_at, p.created_at)))
                                           FROM peers p
                                          WHERE p.device_id = c.id), c.created_at)
                           ) < now() - make_interval(days => $1::int)
                     ORDER BY c.created_at
                     LIMIT $2
                )
                RETURNING id, subscription_id, device_uid, device_name, public_key, platform,
                          support_tag, created_at, rotated_at
                "#,
                &[&(days as i32), &limit],
            )
            .await?;

        Ok(rows.iter().map(device_from_row).collect())
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

    /// Live tunnels the cleanup job may revoke, oldest traffic first.
    ///
    /// One query per pass, capped by `limit`: the job revokes what it gets, so
    /// a long-lived deployment drains in batches instead of locking `peers`
    /// with one giant UPDATE. The row carries everything the pure decision
    /// needs — the last booked traffic (falling back to `created_at`), the
    /// subscription's state and whether the grant of this env is revoked.
    pub async fn peers_for_cleanup(
        &self,
        cutoff: DateTime<Utc>,
        now: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<PeerCleanupRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT p.id, p.device_id, p.subscription_id, p.env, p.proto, p.public_key,
                       host(p.address) AS address, p.pool, p.label,
                       p.created_at, p.revoked_at, p.revoke_reason,
                       GREATEST(
                           COALESCE((SELECT max(t.updated_at) FROM peer_traffic t
                                      WHERE t.peer_id = p.id), p.created_at),
                           COALESCE((SELECT w.updated_at FROM peer_watermarks w
                                      WHERE w.peer_id = p.id), p.created_at)
                       ) AS last_seen,
                       (SELECT max(t.updated_at) FROM peer_traffic t
                         WHERE t.peer_id = p.id) AS last_traffic_at,
                       s.is_deleted AS sub_deleted,
                       (s.expires_at IS NOT NULL AND s.expires_at <= $2) AS sub_expired,
                       (COALESCE(s.scope_env = p.env::text, false)) AS env_owned,
                       EXISTS (SELECT 1 FROM subscription_access g
                                WHERE g.subscription_id = p.subscription_id
                                  AND g.env = p.env::text
                                  AND g.revoked_at IS NULL) AS grant_live,
                       EXISTS (SELECT 1 FROM subscription_access g
                                WHERE g.subscription_id = p.subscription_id
                                  AND g.env = p.env::text) AS grant_seen
                  FROM peers p
                  JOIN subscriptions s ON s.id = p.subscription_id
                 WHERE p.revoked_at IS NULL
                   AND (
                        s.is_deleted
                     OR (s.expires_at IS NOT NULL AND s.expires_at <= $2
                         AND NOT EXISTS (SELECT 1 FROM subscription_access g
                                          WHERE g.subscription_id = p.subscription_id
                                            AND g.env = p.env::text
                                            AND g.revoked_at IS NULL))
                     OR (EXISTS (SELECT 1 FROM subscription_access g
                                  WHERE g.subscription_id = p.subscription_id
                                    AND g.env = p.env::text
                                    AND g.revoked_at IS NOT NULL)
                         AND NOT EXISTS (SELECT 1 FROM subscription_access g
                                          WHERE g.subscription_id = p.subscription_id
                                            AND g.env = p.env::text
                                            AND g.revoked_at IS NULL))
                     OR GREATEST(
                          COALESCE((SELECT max(t.updated_at) FROM peer_traffic t
                                     WHERE t.peer_id = p.id), p.created_at),
                          COALESCE((SELECT w.updated_at FROM peer_watermarks w
                                     WHERE w.peer_id = p.id), p.created_at)
                        ) <= $1
                   )
                 ORDER BY last_seen ASC
                 LIMIT $3
                "#,
                &[&cutoff, &now, &limit],
            )
            .await?;

        Ok(rows
            .iter()
            .map(|row| PeerCleanupRow {
                peer: peer_from_row(row),
                last_traffic_at: row.get("last_traffic_at"),
                sub_deleted: row.get("sub_deleted"),
                sub_expired: row.get("sub_expired"),
                env_owned: row.get("env_owned"),
                grant_live: row.get("grant_live"),
                grant_seen: row.get("grant_seen"),
            })
            .collect())
    }

    /// Accumulate peer traffic for one window: advance the peer's watermark and
    /// add the window's bytes to `peer_traffic` in a single transaction.
    ///
    /// Both rows the connection path writes are written here as well — `day`
    /// (the canonical lifetime total the card's `used_bytes` reads) and
    /// `month` (`/sub/` and the traffic screen). Returns `false` without
    /// touching anything when the watermark has already moved past `from`:
    /// that is what makes a replay of the same window (a crash between the two
    /// writes, two api instances, an in-memory origin reset) a no-op instead of
    /// a double count.
    pub async fn book_peer_traffic(
        &self,
        peer_id: Uuid,
        subscription_id: Uuid,
        env: &Env,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        segments: &[crate::traffic::SegmentTraffic],
    ) -> Result<bool> {
        let rows = crate::traffic::booking_rows(segments);
        let total_up: i64 = segments.iter().map(|s| s.uplink as i64).sum();
        let total_down: i64 = segments.iter().map(|s| s.downlink as i64).sum();

        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let tx = client.transaction().await?;

        let advanced = tx
            .execute(
                r#"
                INSERT INTO peer_watermarks
                    (peer_id, subscription_id, env, uplink, downlink, last_persist_at)
                VALUES ($1, $2, $3, $4, $5, $6)
                ON CONFLICT (peer_id) DO UPDATE
                SET uplink = peer_watermarks.uplink + EXCLUDED.uplink,
                    downlink = peer_watermarks.downlink + EXCLUDED.downlink,
                    last_persist_at = EXCLUDED.last_persist_at,
                    updated_at = now()
                WHERE peer_watermarks.last_persist_at <= $7
                "#,
                &[
                    &peer_id,
                    &subscription_id,
                    &env.to_string(),
                    &total_up,
                    &total_down,
                    &to,
                    &from,
                ],
            )
            .await?;

        if advanced == 0 {
            // Another tick (or the previous instance) already booked this
            // window. Nothing to add.
            tx.rollback().await?;
            return Ok(false);
        }

        for row in &rows {
            tx.execute(
                r#"
                INSERT INTO peer_traffic
                    (peer_id, subscription_id, env, period, bucket, uplink_bytes, downlink_bytes)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (peer_id, period, bucket) DO UPDATE
                SET uplink_bytes = peer_traffic.uplink_bytes + EXCLUDED.uplink_bytes,
                    downlink_bytes = peer_traffic.downlink_bytes + EXCLUDED.downlink_bytes,
                    updated_at = now()
                "#,
                &[
                    &peer_id,
                    &subscription_id,
                    &env.to_string(),
                    &row.period,
                    &row.bucket,
                    &(row.uplink as i64),
                    &(row.downlink as i64),
                ],
            )
            .await?;
        }

        tx.commit().await?;

        Ok(true)
    }

    /// Watermark of every peer the persister has booked at least once. A peer
    /// missing from this list has no row yet and starts from its `created_at`.
    pub async fn all_peer_watermarks(&self) -> Result<Vec<PeerWatermark>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT peer_id, subscription_id, env, uplink, downlink, last_persist_at
                FROM peer_watermarks
                "#,
                &[],
            )
            .await?;

        Ok(rows.iter().map(peer_watermark_from_row).collect())
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
        pool: Option<&str>,
        label: Option<&str>,
    ) -> Result<Option<PeerRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let host = format!("{}/32", address);
        // The insert and the subscription's plane marker are one statement:
        // every create path (`POST /v1/peers` and `POST /v1/peers/prepare`)
        // funnels through here, and a crash between two round-trips would
        // leave a live peer on a subscription still counted as v1. The marker
        // CTE runs only when the insert actually returned a row (`ON CONFLICT
        // DO NOTHING`), and only on a subscription that is still v1 — a
        // concurrent create and an already migrated row both no-op.
        let row = client
            .query_opt(
                &format!(
                    r#"
                    WITH ins AS (
                        INSERT INTO peers
                            (device_id, subscription_id, env, proto, public_key, address, pool, label)
                        VALUES ($1, $2, $3, $4, $5, $6::text::cidr, $7, $8)
                        ON CONFLICT DO NOTHING
                        RETURNING *
                    ), mark AS (
                        UPDATE subscriptions s
                           SET plane = CASE
                                         WHEN EXISTS (
                                             SELECT 1 FROM connections c
                                              WHERE c.subscription_id = s.id
                                                AND c.is_deleted = false)
                                         THEN 'dual' ELSE 'v2' END,
                               v2_since = COALESCE(s.v2_since, now())
                         WHERE s.id = $2
                           AND (s.plane IS NULL OR s.plane = 'v1')
                           AND EXISTS (SELECT 1 FROM ins)
                    )
                    SELECT {PEER_COLUMNS} FROM ins
                    "#
                ),
                &[
                    &device_id,
                    &subscription_id,
                    &env.to_string(),
                    &proto,
                    &public_key,
                    &host,
                    &pool,
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

    // --------------------------------------------------------------- expiry

    /// Live grants whose term is already over but which were never revoked.
    ///
    /// Nothing else removes them: their peers stay programmed on the nodes and
    /// keep the tunnel up forever, so this list is what the expiry task eats.
    pub async fn expired_grants(&self, now: DateTime<Utc>) -> Result<Vec<(Uuid, Env)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT subscription_id, env
                  FROM subscription_access
                 WHERE revoked_at IS NULL
                   AND expires_at IS NOT NULL
                   AND expires_at <= $1
                "#,
                &[&now],
            )
            .await?;

        Ok(rows
            .iter()
            .filter_map(|row| {
                let raw: String = row.get("env");
                match Env::from_str(&raw) {
                    Ok(env) => Some((row.get::<_, Uuid>("subscription_id"), env)),
                    Err(_) => {
                        tracing::warn!("peers: grant of a bad env {raw:?} is not expirable");
                        None
                    }
                }
            })
            .collect())
    }

    /// Live grants whose traffic limit is already reached. The aggregate is the
    /// same one `used_bytes` reports (daily rows), so a peer tunnel is cut at
    /// exactly the number the client was shown.
    pub async fn over_quota_grants(&self) -> Result<Vec<(Uuid, Env)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT g.subscription_id, g.env
                  FROM subscription_access g
                 WHERE g.revoked_at IS NULL
                   AND g.traffic_limit IS NOT NULL
                   AND COALESCE((
                        SELECT SUM(t.uplink_bytes + t.downlink_bytes)
                          FROM subscription_traffic t
                         WHERE t.subscription_id = g.subscription_id
                           AND t.env = g.env
                           AND t.period = 'day'
                       ), 0) >= g.traffic_limit
                "#,
                &[],
            )
            .await?;

        Ok(rows
            .iter()
            .filter_map(|row| {
                let raw: String = row.get("env");
                match Env::from_str(&raw) {
                    Ok(env) => Some((row.get::<_, Uuid>("subscription_id"), env)),
                    Err(_) => None,
                }
            })
            .collect())
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
                            host(address) AS address, pool, label, created_at, revoked_at, \
                            revoke_reason";

/// One plan's shard load: live grants on the env and the shard's limit
/// (`0` = no declared cap).
async fn shard_load(
    tx: &tokio_postgres::Transaction<'_>,
    env: &str,
    plan: &str,
    shards: &[(String, i64)],
) -> Result<(i64, i64, bool)> {
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
        .find(|(candidate, _)| candidate == env)
        .map(|(_, limit)| *limit)
        .unwrap_or(0);
    Ok((used, limit, limit > 0 && used > limit))
}

/// Sticky shard for a plan: one the subscription already holds, otherwise the
/// least loaded of `shards` (room first, then fill ratio). `None` = nothing can
/// take this plan at all.
///
/// Runs inside the caller's transaction so the choice and the write are one
/// step: two concurrent buyers of the same plan must not read the same count.
async fn pick_shard(
    tx: &tokio_postgres::Transaction<'_>,
    subscription_id: Uuid,
    plan: &str,
    shards: &[(String, i64)],
) -> Result<Option<String>> {
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

    if let Some(row) = existing.first() {
        return Ok(Some(row.get::<_, String>("env")));
    }

    // Shards with room first, then the least loaded of them by fill ratio — a
    // shard whose users were revoked is filled again at once, because only live
    // grants are counted. A shard at its own `max_subscriptions` is a soft cap:
    // it is skipped while any other shard has room, and only an all-full plan
    // lands here.
    let mut best: Option<(String, f64)> = None;
    let mut overflow_pick: Option<(String, i64)> = None;

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

        if *limit > 0 && used >= *limit {
            let better = match &overflow_pick {
                None => true,
                Some((env, used_best)) => {
                    used < *used_best || (used == *used_best && candidate < env)
                }
            };
            if better {
                overflow_pick = Some((candidate.clone(), used));
            }
            continue;
        }

        let ratio = if *limit > 0 {
            used as f64 / *limit as f64
        } else {
            // No cap on this shard: compare absolute counts, which is the same
            // ordering with an unknown denominator.
            used as f64
        };
        let better = match &best {
            None => true,
            Some((env, best_ratio)) => {
                ratio < *best_ratio || (ratio == *best_ratio && candidate < env)
            }
        };
        if better {
            best = Some((candidate.clone(), ratio));
        }
    }

    Ok(best
        .map(|(env, _)| env)
        .or_else(|| overflow_pick.map(|(env, _)| env)))
}

/// Narrow a requested protocol list to what the picked shard actually serves.
///
/// A shard declares its protocols through the inbounds its serving nodes
/// announce — nothing in the config lists them. A tag the env does not serve
/// would otherwise sit in the grant forever (the upsert only unions), promising
/// cards no node can build and letting the v1 plane mint configs nobody answers.
///
/// Fails open twice on purpose: an env missing from `announced`, or one that
/// announces nothing, means the mesh is not loaded (or every machine is down).
/// Stripping the grant then would take a paying user's rights away for a
/// transient condition, so the request is kept as-is.
pub(crate) fn filter_protos_for_env(
    protos: &[Tag],
    env: &str,
    announced: &HashMap<String, HashSet<Tag>>,
) -> Vec<Tag> {
    match announced.get(env) {
        Some(tags) if !tags.is_empty() => {
            let kept: Vec<Tag> = protos
                .iter()
                .copied()
                .filter(|tag| tags.contains(tag))
                .collect();
            if kept.is_empty() {
                protos.to_vec()
            } else {
                kept
            }
        }
        _ => protos.to_vec(),
    }
}

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
        rotated_at: row.get("rotated_at"),
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
        pool: row.get("pool"),
        label: row.get("label"),
        created_at: row.get("created_at"),
        revoked_at: row.get("revoked_at"),
        revoke_reason: row.get("revoke_reason"),
    }
}

fn peer_watermark_from_row(row: &tokio_postgres::Row) -> PeerWatermark {
    let env: String = row.get("env");
    PeerWatermark {
        peer_id: row.get("peer_id"),
        subscription_id: row.get("subscription_id"),
        env: Env::from_str(env.as_str()).unwrap_or(Env::Custom(env)),
        uplink: row.get("uplink"),
        downlink: row.get("downlink"),
        last_persist_at: row.get("last_persist_at"),
    }
}

#[cfg(test)]
mod grant_filter_tests {
    use super::filter_protos_for_env;
    use fcore::Tag;
    use std::collections::{HashMap, HashSet};

    fn announced(pairs: &[(&str, &[Tag])]) -> HashMap<String, HashSet<Tag>> {
        pairs
            .iter()
            .map(|(env, tags)| (env.to_string(), tags.iter().copied().collect()))
            .collect()
    }

    /// The junk that started this: a key activation carries every protocol the
    /// catalogue names, while the shard only serves peer tunnels. The grant must
    /// come out with the peer tags alone, never with vless/h2 nobody answers.
    #[test]
    fn a_peer_only_shard_drops_the_v1_protocols() {
        let asked = [
            Tag::AmneziaPeer,
            Tag::WireguardPeer,
            Tag::Hysteria2,
            Tag::VlessTcpReality,
            Tag::VlessGrpcReality,
            Tag::VlessXhttpCdn,
            Tag::Wireguard,
            Tag::AmneziaWg,
        ];
        let mesh = announced(&[("0001", &[Tag::AmneziaPeer, Tag::WireguardPeer])]);

        let kept = filter_protos_for_env(&asked, "0001", &mesh);

        assert_eq!(kept, vec![Tag::AmneziaPeer, Tag::WireguardPeer]);
    }

    /// A shard that starts announcing a proxy protocol keeps it without any
    /// config edit: this is what makes a new pool self-declaring.
    #[test]
    fn a_newly_announced_protocol_survives_the_filter() {
        let asked = [Tag::AmneziaPeer, Tag::Hysteria2];
        let mesh = announced(&[("0001", &[Tag::AmneziaPeer, Tag::Hysteria2])]);

        let kept = filter_protos_for_env(&asked, "0001", &mesh);

        assert_eq!(kept, vec![Tag::AmneziaPeer, Tag::Hysteria2]);
    }

    /// Cold start: the mesh snapshot has not loaded, so `announced` is empty.
    /// Stripping the grant then would take rights away from a payer for a
    /// transient condition, so the request must pass through untouched.
    #[test]
    fn an_empty_mesh_never_strips_a_grant() {
        let asked = [Tag::AmneziaPeer, Tag::Hysteria2];
        let kept = filter_protos_for_env(&asked, "0001", &HashMap::new());
        assert_eq!(kept, asked.to_vec());
    }

    /// Every node of the env is down: same reasoning as the cold start.
    #[test]
    fn an_env_that_announces_nothing_keeps_the_request() {
        let asked = [Tag::AmneziaPeer, Tag::Hysteria2];
        let mesh = announced(&[("0001", &[])]);
        let kept = filter_protos_for_env(&asked, "0001", &mesh);
        assert_eq!(kept, asked.to_vec());
    }

    /// Nothing requested is served (a shard picked for the wrong plan): keep the
    /// request rather than write an empty `protos`, which would leave the
    /// subscription with no rights at all.
    #[test]
    fn a_total_mismatch_keeps_the_request() {
        let asked = [Tag::Hysteria2];
        let mesh = announced(&[("0001", &[Tag::AmneziaPeer])]);
        let kept = filter_protos_for_env(&asked, "0001", &mesh);
        assert_eq!(kept, asked.to_vec());
    }
}
