//! Peer HTTP surface: locations, tunnels, devices, grants and the support
//! lookup.
//!
//! Validation order is deliberate and lives in `crate::peers`: grant →
//! limits → key → address. The database only stores and refuses races (unique
//! indexes), it is never the validator: an address is picked from the blocks
//! in memory, and a `ON CONFLICT DO NOTHING` result means "take the next
//! address", not an error the user sees.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicI64, Ordering};

use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use warp::http::StatusCode;

use fcore::{
    http::ResponseMessage, Connection, ConnectionApiOperations, ConnectionBaseOperations, Env,
    NodeStorageOperations, SubscriptionOperations, SubscriptionStorageOperations,
};

use crate::peers::alloc;
use crate::peers::{self, Grant, Limits, PeerProto, Refusal};
use crate::postgres::peers::{AccessRow, Inserted, PeerRow};
use crate::sync::MemSync;

const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Monotonic counter for support tags: the random part makes collisions
/// unlikely, the counter makes them impossible inside one process, and the
/// unique index catches the remaining cross-process case.
static SUPPORT_SEQ: AtomicI64 = AtomicI64::new(0);

fn support_tag() -> String {
    let mut rng = rand::thread_rng();
    let seq = SUPPORT_SEQ.fetch_add(1, Ordering::Relaxed) as u64;
    let mut out = String::with_capacity(12);
    for i in 0..10usize {
        let byte = if i < 8 {
            rng.gen::<u8>()
        } else {
            ((seq >> ((i - 8) * 8)) & 0xff) as u8
        };
        if i == 4 || i == 8 {
            out.push('-');
        }
        out.push(CROCKFORD[(byte % 32) as usize] as char);
    }
    out
}

fn json_reply<T: Serialize>(status: u16, message: &str, payload: T) -> Box<dyn warp::Reply> {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = ResponseMessage {
        status,
        message: message.to_string(),
        response: payload,
    };
    Box::new(warp::reply::with_status(warp::reply::json(&body), code))
}

fn refusal_reply(refusal: Refusal) -> Box<dyn warp::Reply> {
    #[derive(Serialize)]
    struct ErrorBody {
        error: &'static str,
        detail: Option<i64>,
    }

    let detail = match refusal {
        Refusal::DeviceLimit(limit) | Refusal::SubscriptionLimit(limit) => Some(limit),
        _ => None,
    };
    let body = ErrorBody {
        error: refusal.code(),
        detail,
    };
    json_reply(refusal.status(), refusal.code(), body)
}

fn db_error(context: &str, error: impl std::fmt::Display) -> Box<dyn warp::Reply> {
    tracing::error!("peers: {} failed: {}", context, error);
    json_reply(500, "database error", serde_json::json!({ "error": "db" }))
}

// ---------------------------------------------------------------- requests

#[derive(Clone, Debug, Deserialize)]
pub struct PeerCreateRequest {
    pub subscription_id: Uuid,
    pub device_uid: Uuid,
    pub device_name: Option<String>,
    pub env: String,
    pub proto: String,
    pub public_key: Option<String>,
    pub label: Option<String>,
}

/// `POST /v1/peers/list` — the reconnect case: which tunnels exist.
#[derive(Clone, Debug, Deserialize)]
pub struct PeerListRequest {
    pub subscription_id: Uuid,
    #[serde(default)]
    pub device_uid: Option<Uuid>,
}

/// `POST /v1/peers/delete` — the id rides in the body, not in the path, so the
/// whole request fits one encrypted envelope.
#[derive(Clone, Debug, Deserialize)]
pub struct PeerDeleteRequest {
    pub subscription_id: Uuid,
    pub peer_id: Uuid,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DeviceListRequest {
    pub subscription_id: Uuid,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DeviceUpdateRequest {
    pub subscription_id: Uuid,
    pub device_id: Uuid,
    pub device_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DeviceDeleteRequest {
    pub subscription_id: Uuid,
    pub device_id: Uuid,
}

/// `POST /v1/locations` — what the subscription may use and where.
#[derive(Clone, Debug, Deserialize)]
pub struct LocationsRequest {
    pub subscription_id: Uuid,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AccessUpsertRequest {
    pub subscription_id: Uuid,
    /// Fixed shard. Omit together with `plan` to let the api assign one.
    pub env: Option<String>,
    /// Assignment mode: keep the shard this subscription already has for the
    /// plan, otherwise take the least-loaded one.
    pub plan: Option<String>,
    /// Protocol tags of the grant, snake_case (`amnezia_peer`, `vless_xhttp_cdn`, ...).
    pub protos: Vec<String>,
    #[serde(default)]
    pub source: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub traffic_limit: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AccessRevokeRequest {
    pub subscription_id: Uuid,
    pub env: String,
}

// --------------------------------------------------------------- responses

#[derive(Clone, Debug, Serialize)]
pub struct PoolNode {
    pub endpoint: String,
    pub server_public_key: String,
    pub mtu: u16,
    pub dns: Vec<String>,
    pub keepalive: Option<u16>,
    pub obfuscation: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PoolCard {
    pub env: String,
    pub pool: String,
    pub pool_translation: Option<std::collections::BTreeMap<String, String>>,
    pub proto: String,
    pub nodes: Vec<PoolNode>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PeerView {
    pub peer_id: Uuid,
    pub device_id: Uuid,
    pub env: String,
    pub proto: String,
    pub address: String,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceView {
    pub id: Uuid,
    pub device_uid: Uuid,
    pub device_name: Option<String>,
    pub support_tag: String,
    pub peers: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AccessView {
    pub env: String,
    /// Machine value (`lite`); the app shows `plan_title` instead.
    pub plan: Option<String>,
    /// Human name. "Super Duper" for `lite`: the product is renamed in the UI
    /// only, so no migration and no old rows to touch.
    pub plan_title: String,
    pub protos: Vec<String>,
    pub sources: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub traffic_limit: Option<i64>,
    pub used_bytes: i64,
}

fn peer_view(row: &PeerRow) -> PeerView {
    PeerView {
        peer_id: row.id,
        device_id: row.device_id,
        env: row.env.to_string(),
        proto: row.proto.to_string(),
        address: row.address.clone(),
        label: row.label.clone(),
        created_at: row.created_at,
        revoked_at: row.revoked_at,
        revoke_reason: row.revoke_reason.clone(),
    }
}

/// Display name of a plan. The single place where "ite is Super Duper" lives.
fn plan_title(plan: Option<&str>) -> String {
    match plan {
        Some("standard") => "Standard".to_string(),
        Some("lite") => "Super Duper".to_string(),
        Some("gaming") => "Gaming".to_string(),
        Some("bs-prem") => "BS Premium".to_string(),
        Some(other) => other.to_string(),
        None => "Доступ".to_string(),
    }
}

fn access_view(row: &AccessRow, used_bytes: i64) -> AccessView {
    AccessView {
        env: row.env.to_string(),
        plan: row.plan.clone(),
        plan_title: plan_title(row.plan.as_deref()),
        protos: row.protos.iter().map(|t| t.to_string()).collect(),
        sources: row.sources.clone(),
        expires_at: row.expires_at,
        traffic_limit: row.traffic_limit,
        used_bytes,
    }
}

fn grant_of(row: &AccessRow) -> Grant {
    Grant {
        env: row.env.clone(),
        protos: row.protos.clone(),
        sources: row.sources.clone(),
        expires_at: row.expires_at,
        traffic_limit: row.traffic_limit,
    }
}

// ------------------------------------------------------------------ pools

/// Group the live nodes of one env into the cards the client shows.
///
/// A card is a `pool`: all its nodes share the client-facing parameters
/// (`pool_inbounds_conflict` enforces that at registration), so one config is
/// valid for any node of the card — only the endpoint differs.
fn pool_cards<N: NodeStorageOperations>(nodes: &N, env: &Env, proto: PeerProto) -> Vec<PoolCard> {
    let tag = proto.tag();
    let mut grouped: HashMap<
        String,
        (
            Option<std::collections::BTreeMap<String, String>>,
            Vec<(Uuid, PoolNode)>,
        ),
    > = HashMap::new();

    for (uuid, node) in nodes.iter_nodes() {
        if &node.env != env || !node.status.is_serving() {
            continue;
        }
        let Some(inbound) = node.inbounds.get(&tag) else {
            continue;
        };

        let (server_public_key, mtu, dns, keepalive, obfuscation) = match proto {
            PeerProto::Amnezia => {
                let Some(awg) = inbound.awg.as_ref() else {
                    continue;
                };
                let Ok(pubkey) = awg.interface.private_key.pubkey() else {
                    continue;
                };
                (
                    pubkey,
                    awg.interface.mtu.unwrap_or(1280),
                    awg.interface.dns.iter().map(|ip| ip.to_string()).collect(),
                    awg.keepalive,
                    awg.obfuscation
                        .as_ref()
                        .and_then(|o| serde_json::to_value(o).ok()),
                )
            }
            PeerProto::Wireguard => {
                let Some(wg) = inbound.wg.as_ref() else {
                    continue;
                };
                let Ok(pubkey) = wg.keys.pubkey() else {
                    continue;
                };
                (
                    pubkey,
                    1420,
                    wg.dns.iter().map(|ip| ip.to_string()).collect(),
                    wg.keepalive,
                    None,
                )
            }
        };

        let endpoint = format!("{}:{}", node.address, inbound.port);
        let pool_name = node.pool.clone().unwrap_or_else(|| "default".to_string());
        let entry = grouped
            .entry(pool_name)
            .or_insert_with(|| (node.pool_translation.clone(), Vec::new()));
        entry.1.push((
            *uuid,
            PoolNode {
                endpoint,
                server_public_key,
                mtu,
                dns,
                keepalive,
                obfuscation,
            },
        ));
    }

    let mut cards: Vec<PoolCard> = grouped
        .into_iter()
        .map(|(pool, (translation, mut nodes))| {
            nodes.sort_by_key(|(uuid, _)| *uuid);
            PoolCard {
                env: env.to_string(),
                pool,
                pool_translation: translation,
                proto: proto.as_str().to_string(),
                nodes: nodes.into_iter().map(|(_, node)| node).collect(),
            }
        })
        .collect();
    cards.sort_by(|a, b| a.pool.cmp(&b.pool));
    cards
}

fn cards_for_grants<N: NodeStorageOperations>(nodes: &N, grants: &[AccessRow]) -> Vec<PoolCard> {
    let mut out = Vec::new();
    for grant in grants {
        for proto in [PeerProto::Amnezia, PeerProto::Wireguard] {
            if grant.protos.contains(&proto.tag()) {
                out.extend(pool_cards(nodes, &grant.env, proto));
            }
        }
    }
    out
}

/// `standard-01` → `standard`. The plan is the env prefix before the last
/// dash-number, which is how shards are named.
pub fn plan_of_env(env: &str) -> Option<String> {
    let (head, tail) = env.rsplit_once('-')?;
    if tail.chars().all(|c| c.is_ascii_digit()) {
        Some(head.to_string())
    } else {
        None
    }
}

/// Shards of a plan that can take a subscription right now.
///
/// Only envs with at least one **serving** node announcing a peer interface are
/// returned: assigning into an env whose machines are gone hands out a tunnel
/// nobody serves. The limit is the `env_capacity` override when set, otherwise
/// `nodes * subscriptions_per_node`.
pub(crate) async fn live_shards<N, C, S>(
    memory: &MemSync<N, C, S>,
    plan: &str,
    subscriptions_per_node: i64,
) -> Vec<(String, i64)>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let node_counts: HashMap<String, i64> = {
        let mem = memory.memory.read().await;
        let mut counts: HashMap<String, i64> = HashMap::new();
        for (_, node) in mem.nodes.iter_nodes() {
            if !node.status.is_serving() {
                continue;
            }
            if !node.inbounds.keys().any(|tag| tag.is_peer()) {
                continue;
            }
            *counts.entry(node.env.to_string()).or_default() += 1;
        }
        counts
    };

    let overrides = memory
        .db
        .peers()
        .plan_shards(plan)
        .await
        .unwrap_or_default();

    let mut shards: Vec<(String, i64)> = Vec::new();
    for (env, count) in node_counts {
        let override_row = overrides.iter().find(|(e, _, _)| e == &env);
        if let Some((_, _, false)) = override_row {
            continue; // switched off by the admin
        }
        let limit = match override_row.and_then(|(_, max, _)| *max) {
            Some(max) => max,
            None => (count * subscriptions_per_node).max(1),
        };
        shards.push((env, limit));
    }
    // Deterministic order: least-loaded ties break on the env name.
    shards.sort_by(|a, b| a.0.cmp(&b.0));
    shards
}

// ---------------------------------------------------------------- publish

/// Push one peer change to every node of the env.
///
/// A failure is logged and swallowed: the peer already exists in the database,
/// and the node plane heals on its next start (authoritative `peers-init`),
/// so a transient bus hiccup must not fail the user's request.
async fn publish_peer<N, C, S>(memory: &MemSync<N, C, S>, action: fcore::Action, peer: &PeerRow)
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let host = peer.address.split('/').next().unwrap_or(&peer.address);
    let messages = vec![fcore::PeerMessage {
        peer_id: peer.id,
        action,
        tag: peer.proto,
        public_key: peer.public_key.clone(),
        address: host.to_string(),
        subscription_id: Some(peer.subscription_id),
    }];

    let bytes = match rkyv::to_bytes::<_, 1024>(&messages) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::error!("peers: serialize failed: {}", e);
            return;
        }
    };
    let topic = fcore::Topic::PeersUpdates(peer.env.clone());
    if let Err(e) = memory.publisher.send_binary(&topic, bytes.as_ref()).await {
        tracing::error!("peers: publish {} failed: {}", topic, e);
    }
}

/// Publish the authoritative peer set of an env to one node.
///
/// Called when a node registers: the node subscribes to `peers-init-<uuid>`
/// at start, and without this push it would sit empty until somebody created
/// or deleted a tunnel. An empty set is meaningful — it tells the node to
/// prune everything it still has.
pub async fn publish_peers_init<N, C, S>(
    memory: &MemSync<N, C, S>,
    node_uuid: Uuid,
    env: &Env,
    tags: &[fcore::Tag],
) where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let rows = match memory.db.peers().live_peers_of_env(env).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("peers: init query failed for {}: {}", env, e);
            return;
        }
    };

    let messages: Vec<fcore::PeerMessage> = rows
        .iter()
        .filter(|row| tags.contains(&row.proto))
        .map(|row| {
            let host = row.address.split('/').next().unwrap_or(&row.address);
            fcore::PeerMessage {
                peer_id: row.id,
                action: fcore::Action::Create,
                tag: row.proto,
                public_key: row.public_key.clone(),
                address: host.to_string(),
                subscription_id: Some(row.subscription_id),
            }
        })
        .collect();

    let count = messages.len();
    let bytes = match rkyv::to_bytes::<_, 1024>(&messages) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::error!("peers: init serialize failed: {}", e);
            return;
        }
    };
    let topic = fcore::Topic::PeersInit(node_uuid);
    match memory.publisher.send_binary(&topic, bytes.as_ref()).await {
        Ok(_) => tracing::info!(
            "peers: sent {} tunnels of {} to node {}",
            count,
            env,
            node_uuid
        ),
        Err(e) => tracing::error!("peers: init publish {} failed: {}", topic, e),
    }
}

// ---------------------------------------------------------------- handlers

/// POST /v1/peers
pub async fn post_peers_handler<N, C, S>(
    req: PeerCreateRequest,
    memory: MemSync<N, C, S>,
    default_limits: Limits,
) -> Box<dyn warp::Reply>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let Some(proto) = PeerProto::parse(&req.proto) else {
        return json_reply(
            400,
            "unknown proto",
            serde_json::json!({ "error": "unknown_proto" }),
        );
    };
    let env = match Env::from_str(&req.env) {
        Ok(env) => env,
        Err(_) => {
            return json_reply(400, "bad env", serde_json::json!({ "error": "bad_env" }));
        }
    };

    if let Some(key) = req.public_key.as_deref() {
        if !peers::is_valid_public_key(key) {
            return refusal_reply(Refusal::BadPublicKey);
        }
    }
    if let Some(label) = req.label.as_deref() {
        if !peers::is_valid_label(label) {
            return refusal_reply(Refusal::BadLabel);
        }
    }

    {
        let mem = memory.memory.read().await;
        match mem.subscriptions.find_by_id(&req.subscription_id) {
            Some(sub) if !sub.is_deleted() => {}
            _ => return refusal_reply(Refusal::NoSubscription),
        }
    }

    let db = memory.db.peers();

    // The device is created lazily on the first tunnel of an installation.
    let mut device = match db.device_by_uid(req.subscription_id, req.device_uid).await {
        Ok(device) => device,
        Err(e) => return db_error("device_by_uid", e),
    };

    if device.is_none() {
        let Some(key) = req.public_key.as_deref() else {
            return refusal_reply(Refusal::BadPublicKey);
        };
        let mut created = false;
        for _ in 0..5 {
            let tag = support_tag();
            match db
                .insert_device(
                    req.subscription_id,
                    req.device_uid,
                    req.device_name.as_deref(),
                    key,
                    None,
                    &tag,
                )
                .await
            {
                Ok(Inserted::Done) => {
                    created = true;
                    break;
                }
                Ok(Inserted::Conflict) => {
                    // Either the support tag or (subscription, device_uid)
                    // collided. A concurrent create of the same device is not
                    // an error: adopt the existing row.
                    if let Ok(Some(existing)) =
                        db.device_by_uid(req.subscription_id, req.device_uid).await
                    {
                        device = Some(existing);
                        break;
                    }
                }
                Err(e) => return db_error("insert_device", e),
            }
        }
        if device.is_none() {
            if !created {
                return json_reply(
                    409,
                    "support tag collision",
                    serde_json::json!({ "error": "support_tag_taken" }),
                );
            }
            match db.device_by_uid(req.subscription_id, req.device_uid).await {
                Ok(existing) => device = existing,
                Err(e) => return db_error("device_by_uid", e),
            }
        }
    }

    let device = match device {
        Some(device) => device,
        None => return refusal_reply(Refusal::DeviceNotFound),
    };

    // Grant decides whether this env/protocol is allowed at all.
    let grant = {
        let mem = memory.memory.read().await;
        mem.peers_state.grant(&req.subscription_id, &env).cloned()
    };
    let now = Utc::now();
    if let Err(refusal) = peers::check_grant(grant.as_ref().map(grant_of).as_ref(), proto, now) {
        return refusal_reply(refusal);
    }
    let grant = grant.expect("checked above");

    if let Some(limit) = grant.traffic_limit {
        match db.used_bytes(req.subscription_id, &env).await {
            Ok(used) if used >= limit => return refusal_reply(Refusal::QuotaExhausted),
            Ok(_) => {}
            Err(e) => return db_error("used_bytes", e),
        }
    }

    let (per_device, per_subscription) = {
        let mem = memory.memory.read().await;
        mem.peers_state.limits_for(&req.subscription_id)
    };
    let limits = Limits::resolve(default_limits, per_device, per_subscription);
    let (device_live, sub_live) = {
        let mem = memory.memory.read().await;
        mem.peers_state.peers.counts(&req.subscription_id, &device.id)
    };
    if let Err(refusal) = limits.check_device(device_live as i64) {
        return refusal_reply(refusal);
    }
    if let Err(refusal) = limits.check_subscription(sub_live as i64) {
        return refusal_reply(refusal);
    }

    let public_key = req
        .public_key
        .clone()
        .unwrap_or_else(|| device.public_key.clone());

    // Idempotency: the same key in the same env is the same tunnel.
    let existing = {
        let mem = memory.memory.read().await;
        mem.peers_state
            .peers
            .live_by_key(&env, &public_key)
            .cloned()
    };
    match existing {
        Some(existing) if existing.subscription_id == req.subscription_id => {
            let cards = {
                let mem = memory.memory.read().await;
                pool_cards(&mem.nodes, &env, proto)
            };
            return json_reply(
                200,
                "peer exists",
                serde_json::json!({
                    "peer": peer_view(&existing),
                    "pools": cards,
                }),
            );
        }
        Some(_) => return refusal_reply(Refusal::KeyTaken),
        None => {}
    }

    let (blocks, mut used) = {
        let mem = memory.memory.read().await;
        (
            mem.peers_state.block_masks(&env),
            mem.peers_state.peers.used_in(&env),
        )
    };
    if blocks.is_empty() {
        return refusal_reply(Refusal::EnvNotServed);
    }

    let mut created: Option<PeerRow> = None;
    for attempt in 0..3 {
        let Some(address) = alloc::first_free(&blocks, &used) else {
            return refusal_reply(Refusal::NoFreeAddress);
        };
        match db
            .insert_peer(
                device.id,
                req.subscription_id,
                &env,
                proto.tag(),
                &public_key,
                address,
                req.label.as_deref(),
            )
            .await
        {
            Ok(Some(row)) => {
                created = Some(row);
                break;
            }
            Ok(None) => {
                // Either the address or the label was taken between our read
                // and the insert: retry with the next address.
                tracing::info!(
                    "peers: insert conflict (attempt {}) for {} in {}",
                    attempt,
                    public_key,
                    env
                );
                used.insert(address);
            }
            Err(e) => return db_error("insert_peer", e),
        }
    }

    let Some(peer) = created else {
        return json_reply(
            409,
            "could not allocate a peer",
            serde_json::json!({ "error": "conflict" }),
        );
    };

    // Write-through: Postgres holds the row, memory gets the same one, so the
    // next request sees this tunnel without a query.
    memory
        .memory
        .write()
        .await
        .peers_state
        .peers
        .insert(peer.clone());

    publish_peer(&memory, fcore::Action::Create, &peer).await;

    let cards = {
        let mem = memory.memory.read().await;
        pool_cards(&mem.nodes, &env, proto)
    };

    json_reply(
        200,
        "peer created",
        serde_json::json!({
            "peer": peer_view(&peer),
            "support_tag": device.support_tag,
            "pools": cards,
        }),
    )
}

/// `POST /v1/peers/list`
pub async fn get_peers_handler<N, C, S>(
    req: PeerListRequest,
    memory: MemSync<N, C, S>,
    remember_days: i64,
) -> Box<dyn warp::Reply>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let db = memory.db.peers();
    let subscription_id = req.subscription_id;
    let device_filter = match req.device_uid {
        Some(uid) => match db.device_by_uid(subscription_id, uid).await {
            Ok(Some(device)) => Some(device.id),
            Ok(None) => return refusal_reply(Refusal::DeviceNotFound),
            Err(e) => return db_error("device_by_uid", e),
        },
        None => None,
    };

    match db
        .peers_of(subscription_id, device_filter, remember_days)
        .await
    {
        Ok(rows) => {
            let peers: Vec<PeerView> = rows.iter().map(peer_view).collect();
            json_reply(200, "ok", serde_json::json!({ "peers": peers }))
        }
        Err(e) => db_error("peers_of", e),
    }
}

/// `POST /v1/peers/delete`
pub async fn delete_peer_handler<N, C, S>(
    req: PeerDeleteRequest,
    memory: MemSync<N, C, S>,
) -> Box<dyn warp::Reply>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let db = memory.db.peers();
    let peer_id = req.peer_id;
    let peer = {
        let mem = memory.memory.read().await;
        mem.peers_state.peers.get(&peer_id).cloned()
    };
    match peer {
        Some(peer) if peer.subscription_id == req.subscription_id => {
            match db.revoke_peer(peer_id, "user").await {
                Ok(true) => {
                    memory
                        .memory
                        .write()
                        .await
                        .peers_state
                        .peers
                        .remove_live(peer_id);
                    publish_peer(&memory, fcore::Action::Delete, &peer).await;
                    json_reply(
                        200,
                        "peer revoked",
                        serde_json::json!({ "peer_id": peer_id }),
                    )
                }
                Ok(false) => refusal_reply(Refusal::PeerNotFound),
                Err(e) => db_error("revoke_peer", e),
            }
        }
        Some(_) => refusal_reply(Refusal::PeerNotFound),
        None => refusal_reply(Refusal::PeerNotFound),
    }
}

/// `POST /v1/devices/list`
pub async fn get_devices_handler<N, C, S>(
    req: DeviceListRequest,
    memory: MemSync<N, C, S>,
) -> Box<dyn warp::Reply>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    match memory.db.peers().devices(req.subscription_id).await {
        Ok(rows) => {
            let devices: Vec<DeviceView> = rows
                .into_iter()
                .map(|(row, live)| DeviceView {
                    id: row.id,
                    device_uid: row.device_uid,
                    device_name: row.device_name,
                    support_tag: row.support_tag,
                    peers: live,
                })
                .collect();
            json_reply(200, "ok", serde_json::json!({ "devices": devices }))
        }
        Err(e) => db_error("devices", e),
    }
}

/// `POST /v1/devices/update`
pub async fn patch_device_handler<N, C, S>(
    req: DeviceUpdateRequest,
    memory: MemSync<N, C, S>,
) -> Box<dyn warp::Reply>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let db = memory.db.peers();
    let device_id = req.device_id;
    match db.device_by_id(device_id).await {
        Ok(Some(device)) if device.subscription_id == req.subscription_id => {
            match db
                .rename_device(device_id, req.device_name.as_deref())
                .await
            {
                Ok(_) => json_reply(
                    200,
                    "renamed",
                    serde_json::json!({ "device_id": device_id }),
                ),
                Err(e) => db_error("rename_device", e),
            }
        }
        Ok(_) => refusal_reply(Refusal::DeviceNotFound),
        Err(e) => db_error("device_by_id", e),
    }
}

/// `POST /v1/devices/delete`
pub async fn delete_device_handler<N, C, S>(
    req: DeviceDeleteRequest,
    memory: MemSync<N, C, S>,
) -> Box<dyn warp::Reply>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let db = memory.db.peers();
    let device_id = req.device_id;
    match db.device_by_id(device_id).await {
        Ok(Some(device)) if device.subscription_id == req.subscription_id => {
            // Revoke before deleting: the FK cascade drops the rows without
            // publishing, and the nodes would keep the peers until restart.
            let doomed: Vec<PeerRow> = {
                let mem = memory.memory.read().await;
                mem.peers_state
                    .peers
                    .live_of_device(&device_id)
                    .into_iter()
                    .cloned()
                    .collect()
            };
            for peer in &doomed {
                if db.revoke_peer(peer.id, "user").await.is_ok() {
                    memory
                        .memory
                        .write()
                        .await
                        .peers_state
                        .peers
                        .remove_live(peer.id);
                    publish_peer(&memory, fcore::Action::Delete, peer).await;
                }
            }
            match db.delete_device(device_id).await {
                Ok(_) => json_reply(
                    200,
                    "device deleted",
                    serde_json::json!({ "device_id": device_id }),
                ),
                Err(e) => db_error("delete_device", e),
            }
        }
        Ok(_) => refusal_reply(Refusal::DeviceNotFound),
        Err(e) => db_error("device_by_id", e),
    }
}

/// `POST /v1/locations`
pub async fn get_locations_handler<N, C, S>(
    req: LocationsRequest,
    memory: MemSync<N, C, S>,
) -> Box<dyn warp::Reply>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let subscription_id = req.subscription_id;
    let db = memory.db.peers();
    let grants: Vec<AccessRow> = {
        let mem = memory.memory.read().await;
        mem.peers_state
            .access
            .of_sub(&subscription_id)
            .into_iter()
            .cloned()
            .collect()
    };

    let mut access = Vec::with_capacity(grants.len());
    for grant in &grants {
        let used = db
            .used_bytes(subscription_id, &grant.env)
            .await
            .unwrap_or(0);
        access.push(access_view(grant, used));
    }

    let cards = {
        let mem = memory.memory.read().await;
        cards_for_grants(&mem.nodes, &grants)
    };

    json_reply(
        200,
        "ok",
        serde_json::json!({ "access": access, "pools": cards }),
    )
}

/// GET /v1/access?subscription_id=…
pub async fn get_access_handler<N, C, S>(
    subscription_id: Uuid,
    memory: MemSync<N, C, S>,
) -> Box<dyn warp::Reply>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let db = memory.db.peers();
    let grants: Vec<AccessRow> = {
        let mem = memory.memory.read().await;
        mem.peers_state
            .access
            .of_sub(&subscription_id)
            .into_iter()
            .cloned()
            .collect()
    };
    let mut access = Vec::with_capacity(grants.len());
    for grant in &grants {
        let used = db
            .used_bytes(subscription_id, &grant.env)
            .await
            .unwrap_or(0);
        access.push(access_view(grant, used));
    }
    json_reply(200, "ok", serde_json::json!({ "access": access }))
}

/// POST /v1/access (service token): grants the rights the payer bought.
pub async fn post_access_handler<N, C, S>(
    req: AccessUpsertRequest,
    memory: MemSync<N, C, S>,
    subscriptions_per_node: i64,
) -> Box<dyn warp::Reply>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let env = match req.env.as_deref() {
        Some(raw) => match Env::from_str(raw) {
            Ok(env) => Some(env),
            Err(_) => return json_reply(400, "bad env", serde_json::json!({ "error": "bad_env" })),
        },
        None => None,
    };

    let mut protos = Vec::with_capacity(req.protos.len());
    for name in &req.protos {
        match fcore::Tag::from_str(name) {
            Ok(tag) => protos.push(tag),
            Err(_) => {
                return json_reply(
                    400,
                    "unknown proto",
                    serde_json::json!({ "error": "unknown_proto", "proto": name }),
                )
            }
        }
    }
    if protos.is_empty() {
        return json_reply(
            400,
            "empty protos",
            serde_json::json!({ "error": "empty_protos" }),
        );
    }

    let db = memory.db.peers();
    let source = req.source.clone().unwrap_or_else(|| "manual".to_string());

    // Assignment mode: the payer does not pick a shard, the api does. The
    // shard is sticky per plan, and a full house is reported, never refused:
    // a paying user must not hit a wall because of our capacity planning.
    if env.is_none() {
        let Some(plan) = req.plan.clone() else {
            return json_reply(
                400,
                "env or plan is required",
                serde_json::json!({ "error": "env_or_plan_required" }),
            );
        };
        let shards = live_shards(&memory, &plan, subscriptions_per_node).await;
        return match db
            .assign_grant(
                req.subscription_id,
                &plan,
                &protos,
                &source,
                req.expires_at,
                req.traffic_limit,
                &shards,
            )
            .await
        {
            Ok(Some((row, used, limit, overflow))) => {
                if overflow {
                    tracing::error!(
                        "peers: shards of plan '{}' are over capacity ({}/{}), add a shard",
                        plan,
                        used,
                        limit
                    );
                }
                let used_bytes = db
                    .used_bytes(req.subscription_id, &row.env)
                    .await
                    .unwrap_or(0);
                memory
                    .memory
                    .write()
                    .await
                    .peers_state
                    .access
                    .insert(req.subscription_id, row.clone());
                json_reply(
                    200,
                    "grant assigned",
                    serde_json::json!({
                        "access": access_view(&row, used_bytes),
                        "capacity": { "plan": plan, "used": used, "limit": limit, "overflow": overflow },
                    }),
                )
            }
            Ok(None) => json_reply(
                503,
                "no live shard for this plan",
                serde_json::json!({ "error": "no_live_shard", "plan": plan }),
            ),
            Err(e) => db_error("assign_grant", e),
        };
    }
    let env = env.expect("checked above");

    // A grant for an env nobody serves is a typo, not a right: refuse it so
    // the mistake surfaces at the caller instead of at the user's connect
    // button.
    let served = {
        let mem = memory.memory.read().await;
        !mem.peers_state.block_masks(&env).is_empty()
    };
    if !served {
        return refusal_reply(Refusal::EnvNotServed);
    }
    match db
        .upsert_grant(
            req.subscription_id,
            &env,
            &protos,
            &source,
            req.expires_at,
            req.traffic_limit,
        )
        .await
    {
        Ok(row) => {
            let used = db.used_bytes(req.subscription_id, &env).await.unwrap_or(0);
            memory
                .memory
                .write()
                .await
                .peers_state
                .access
                .insert(req.subscription_id, row.clone());
            json_reply(
                200,
                "grant upserted",
                serde_json::json!({ "access": access_view(&row, used) }),
            )
        }
        Err(e) => db_error("upsert_grant", e),
    }
}

/// DELETE /v1/access (service token)
pub async fn delete_access_handler<N, C, S>(
    req: AccessRevokeRequest,
    memory: MemSync<N, C, S>,
) -> Box<dyn warp::Reply>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let env = match Env::from_str(&req.env) {
        Ok(env) => env,
        Err(_) => return json_reply(400, "bad env", serde_json::json!({ "error": "bad_env" })),
    };

    let db = memory.db.peers();
    match db.revoke_grant(req.subscription_id, &env).await {
        Ok(true) => {
            memory
                .memory
                .write()
                .await
                .peers_state
                .access
                .remove(&req.subscription_id, &env);
            // Rights removed means the tunnels go too, immediately: leaving
            // them up until the cleanup task runs would keep a revoked
            // subscription connected.
            let live: Vec<PeerRow> = {
                let mem = memory.memory.read().await;
                mem.peers_state
                    .peers
                    .live_of_sub(&req.subscription_id)
                    .into_iter()
                    .filter(|peer| peer.env == env)
                    .cloned()
                    .collect()
            };
            let revoked = db
                .revoke_env_peers(req.subscription_id, &env, "subscription_ended")
                .await
                .unwrap_or(0);
            memory
                .memory
                .write()
                .await
                .peers_state
                .peers
                .remove_env_of_sub(&req.subscription_id, &env);
            for peer in &live {
                publish_peer(&memory, fcore::Action::Delete, peer).await;
            }
            json_reply(
                200,
                "grant revoked",
                serde_json::json!({ "peers_revoked": revoked }),
            )
        }
        Ok(false) => json_reply(
            404,
            "no live grant",
            serde_json::json!({ "error": "no_grant" }),
        ),
        Err(e) => db_error("revoke_grant", e),
    }
}

/// GET /v1/access/capacity?plan=… (service token): per-shard load.
pub async fn get_capacity_handler<N, C, S>(
    plan: String,
    memory: MemSync<N, C, S>,
) -> Box<dyn warp::Reply>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    match memory.db.peers().capacity_for_plan(&plan).await {
        Ok(rows) => {
            let shards: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|(env, used, limit)| {
                    serde_json::json!({
                        "env": env,
                        "used": used,
                        "limit": limit,
                        "overflow": limit > 0 && used > limit,
                    })
                })
                .collect();
            json_reply(
                200,
                "ok",
                serde_json::json!({ "plan": plan, "shards": shards }),
            )
        }
        Err(e) => db_error("capacity_for_plan", e),
    }
}

/// GET /v1/admin/support/{support_tag} (service token, read-only).
pub async fn get_support_handler<N, C, S>(
    support_tag: String,
    memory: MemSync<N, C, S>,
    remember_days: i64,
) -> Box<dyn warp::Reply>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let db = memory.db.peers();
    let device = match db.device_by_support_tag(&support_tag).await {
        Ok(Some(device)) => device,
        Ok(None) => return refusal_reply(Refusal::DeviceNotFound),
        Err(e) => return db_error("device_by_support_tag", e),
    };

    let peers = match db
        .peers_of(device.subscription_id, Some(device.id), remember_days)
        .await
    {
        Ok(rows) => rows.iter().map(peer_view).collect::<Vec<_>>(),
        Err(e) => return db_error("peers_of", e),
    };

    let grants: Vec<AccessRow> = {
        let mem = memory.memory.read().await;
        mem.peers_state
            .access
            .of_sub(&device.subscription_id)
            .into_iter()
            .cloned()
            .collect()
    };
    let mut access = Vec::with_capacity(grants.len());
    for grant in &grants {
        let used = db
            .used_bytes(device.subscription_id, &grant.env)
            .await
            .unwrap_or(0);
        access.push(access_view(grant, used));
    }

    let subscription = {
        let mem = memory.memory.read().await;
        mem.subscriptions
            .find_by_id(&device.subscription_id)
            .map(|sub| {
                serde_json::json!({
                    "id": sub.id(),
                    "expires_at": sub.expires_at(),
                    "plan_kind": format!("{:?}", sub.plan_kind()),
                    "is_deleted": sub.is_deleted(),
                })
            })
    };

    json_reply(
        200,
        "ok",
        serde_json::json!({
            "device": {
                "id": device.id,
                "device_uid": device.device_uid,
                "device_name": device.device_name,
                "platform": device.platform,
                "support_tag": device.support_tag,
            },
            "subscription": subscription,
            "peers": peers,
            "access": access,
        }),
    )
}

/// Answer for every peer route while the feature is switched off: the routes
/// exist (so a client can probe them) but nothing is served.
pub fn not_enabled() -> Box<dyn warp::Reply> {
    json_reply(
        404,
        "peers are disabled on this api",
        serde_json::json!({ "error": "peers_disabled" }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn support_tags_are_readable_and_unique_enough() {
        let a = support_tag();
        let b = support_tag();
        assert_eq!(a.len(), 12, "XXXX-XXXX-XX");
        assert_eq!(a.chars().filter(|c| *c == '-').count(), 2);
        assert!(a
            .chars()
            .all(|c| c == '-' || CROCKFORD.contains(&(c as u8))));
        assert_ne!(a, b);
    }
}
