//! Peer HTTP surface: locations, tunnels, devices, grants and the support
//! lookup.
//!
//! Validation order is deliberate and lives in `crate::peers`: grant →
//! limits → key → address. The database only stores and refuses races (unique
//! indexes), it is never the validator: an address is picked from the blocks
//! in memory, and a `ON CONFLICT DO NOTHING` result means "take the next
//! address", not an error the user sees.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicI64, Ordering};

use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use warp::http::StatusCode;

use fcore::{
    http::ResponseMessage, Connection, ConnectionApiOperations, ConnectionBaseOperations,
    ConnectionStorageApiOperations, Endpoint, Env, NodeStorageOperations, Subscription,
    SubscriptionOperations, SubscriptionStorageOperations,
};

use crate::peers::alloc;
use crate::peers::prepare::{self, Batch, Decision, PoolTarget};
use crate::peers::{self, Grant, Limits, PeerProto, Refusal};
use crate::postgres::peers::{AccessRow, Inserted, PeerRow};
use crate::sync::tasks::{SyncOp, DELETE_REASON_GRANT_CHANGED};
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
        Refusal::PeersLimit(limit) | Refusal::SubscriptionLimit(limit) => Some(limit),
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
    /// Card the app picked (`<shard>:<pool>:<proto>`). Preferred: the client
    /// never learns the shard id, it only echoes the handle it was given.
    #[serde(default)]
    pub card_id: Option<String>,
    /// Legacy/manual form of the same choice: the shard and the protocol
    /// separately. Kept for curl and for the admin surface.
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub proto: Option<String>,
    pub public_key: Option<String>,
    pub label: Option<String>,
}

/// `POST /v1/peers/prepare` — provision every peer pool of the grant in one
/// call, so the client can store all configs at login/import and run the speed
/// test without connecting first.
///
/// Same envelope and the same key the single create takes; the difference is
/// that one full pool never fails the call: it lands in `skipped` with the code
/// `POST /v1/peers` would have answered for it.
#[derive(Clone, Debug, Deserialize)]
pub struct PeerPrepareRequest {
    pub subscription_id: Uuid,
    pub device_uid: Uuid,
    pub public_key: String,
    pub device_name: Option<String>,
    pub platform: Option<String>,
    /// Only these pools (`<shard>:<pool>`); every peer card of the grant when
    /// absent. A requested handle that matches no card is reported as
    /// `unknown_pool` instead of silently doing nothing.
    #[serde(default)]
    pub pools: Option<Vec<String>>,
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

/// `POST /subscription/{id}/buy` — the cabinet's «Докупить» button.
///
/// The body names nothing but the plan: the subscription comes from the path,
/// which is the same capability handle the cabinet and `GET /subscription/{id}`
/// already use, so a caller cannot buy for a subscription it was not given and
/// cannot pick a price.
#[derive(Clone, Debug, Deserialize)]
pub struct BuyPlanRequest {
    /// `standard` or `lite` (`super-duper` is accepted too).
    pub plan: String,
    /// Traffic package for a lite purchase, in GiB: the gateway prices the
    /// plan by it, exactly like the lite keys of `/pay`.
    #[serde(default)]
    pub traffic_gib: Option<u32>,
}

/// `POST /subscription/{id}/plan` — payment-gw fulfilment of a cabinet
/// purchase. No key is issued for these: the payment itself buys the grant.
#[derive(Clone, Debug, Deserialize)]
pub struct PlanPurchaseRequest {
    /// `standard` or `lite` (`super-duper` is accepted too).
    pub plan: String,
    /// Purchased period in days; 0 for a volume-only Super Duper pack.
    #[serde(default)]
    pub days: i64,
    /// Purchased volume in bytes: added to the plan's quota.
    #[serde(default)]
    pub traffic_bytes: Option<i64>,
    /// Same volume in GiB, for callers that speak the key vocabulary.
    #[serde(default)]
    pub traffic_gib: Option<i64>,
    /// Gateway reference (transaction or trace id): the idempotency key. A
    /// duplicate delivery of the same reference changes nothing.
    #[serde(default, alias = "trace_id", alias = "transaction_id")]
    pub payment_ref: Option<String>,
    /// Source label recorded in `sources`; `payment` when absent.
    #[serde(default)]
    pub source: Option<String>,
    /// Protocol tags of the grant; both peer protocols when absent.
    #[serde(default)]
    pub protos: Option<Vec<String>>,
}

/// One GiB, the unit `POST /key` already speaks.
const GIB: i64 = 1024 * 1024 * 1024;

/// Upper bound of a single purchase's term: ten years. Guards the date
/// arithmetic in `plans::purchase_terms` from absurd payloads.
const MAX_PURCHASE_DAYS: i64 = 3650;

// --------------------------------------------------------------- responses

/// What the client does with a card. `Peer` is the original shape: the app
/// calls `POST /v1/peers` with a key and gets a tunnel. `ClientConfig` cards
/// are rendered from each node's `payload` and never hit the peer endpoint
/// (`docs/v2-card-contract.md`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    /// Absent in the JSON of old responses and old clients: `peer`.
    #[default]
    Peer,
    ClientConfig,
}

/// Protocol a location card is built for. Peer protocols keep the v1 shape;
/// `Hysteria2` and `VlessXhttpCdn` are served as client-built configs: the
/// api hands out the parameters, the app assembles the tunnel itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardProto {
    Peer(PeerProto),
    Hysteria2,
    /// VLESS over xHTTP behind the CDN: plain TLS (no reality), so the whole
    /// client config is described by the node's announced inbound plus the
    /// subscription's existing VLESS uuid.
    VlessXhttpCdn,
}

impl CardProto {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Peer(proto) => proto.as_str(),
            Self::Hysteria2 => "hysteria2",
            Self::VlessXhttpCdn => "vless_xhttp_cdn",
        }
    }

    pub fn tag(self) -> fcore::Tag {
        match self {
            Self::Peer(proto) => proto.tag(),
            Self::Hysteria2 => fcore::Tag::Hysteria2,
            Self::VlessXhttpCdn => fcore::Tag::VlessXhttpCdn,
        }
    }

    pub fn kind(self) -> CardKind {
        match self {
            Self::Peer(_) => CardKind::Peer,
            Self::Hysteria2 | Self::VlessXhttpCdn => CardKind::ClientConfig,
        }
    }

    /// Both spellings of every card protocol: peer protocols keep their
    /// historic aliases (`amnezia`), client_config protocols arrive as the
    /// contract name (`hysteria2`, `vless_xhttp_cdn`).
    pub fn parse(value: &str) -> Option<Self> {
        if let Some(proto) = PeerProto::parse(value) {
            return Some(Self::Peer(proto));
        }
        match value {
            "hysteria2" | "Hysteria2" => Some(Self::Hysteria2),
            "vless_xhttp_cdn" | "VlessXhttpCdn" => Some(Self::VlessXhttpCdn),
            _ => None,
        }
    }

    pub fn from_tag(tag: fcore::Tag) -> Option<Self> {
        PeerProto::from_tag(tag)
            .map(Self::Peer)
            .or_else(|| (tag == fcore::Tag::Hysteria2).then_some(Self::Hysteria2))
            .or_else(|| (tag == fcore::Tag::VlessXhttpCdn).then_some(Self::VlessXhttpCdn))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolNode {
    pub endpoint: String,
    /// ISO country of the machine (`[node] country`, e.g. `NL`). A pool may
    /// span countries, so this rides on the node, not on the card: the app
    /// takes the flag from here instead of parsing the hostname.
    pub country: String,
    pub server_public_key: String,
    pub mtu: u16,
    pub dns: Vec<String>,
    pub keepalive: Option<u16>,
    pub obfuscation: Option<serde_json::Value>,
    /// Client parameters of a `client_config` card (hysteria2 today). Opaque
    /// to the api; absent on peer cards, so their JSON stays as it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// The same entry point in the ONE `[[node.endpoints]]` shape: `ip` +
    /// `port` + `weight` (+ `label`). One record per card row, mirroring
    /// `endpoint` — a client that reads `endpoints` gains the weight, one that
    /// reads `endpoint` stays byte-compatible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<Endpoint>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolCard {
    /// Opaque handle of this card: `<shard>:<pool>:<proto>`. The app echoes it
    /// back in `POST /v1/peers`; the shard id is not a field the client reads.
    pub card_id: String,
    pub pool: String,
    pub pool_translation: Option<std::collections::BTreeMap<String, String>>,
    /// Country under the card's flag. It comes from the pool itself
    /// (`[node] pool_country_code`), because one card may span countries; a
    /// pool that declares none falls back to the country of its first node.
    pub country: Option<String>,
    /// Human name of `country` (`[node] pool_country_name`); absent means the
    /// client translates the code.
    pub country_name: Option<String>,
    pub proto: String,
    /// Absent in old responses and old clients, which read it as `peer`.
    #[serde(default)]
    pub kind: CardKind,
    pub nodes: Vec<PoolNode>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PeerView {
    pub peer_id: Uuid,
    pub device_id: Uuid,
    pub proto: String,
    /// The card this tunnel belongs to (`<shard>:<pool>:<proto>`), so the app
    /// can rebuild the config from the peer list alone: the address comes from
    /// the shard, but the endpoint, the server key and the obfuscation come
    /// from the pool. `None` for tunnels created before `peers.pool` existed
    /// and for the manual `env` + `proto` form — the app then falls back to
    /// `GET /v1/locations`.
    pub card_id: Option<String>,
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
    pub created_at: DateTime<Utc>,
    pub rotated_at: Option<DateTime<Utc>>,
    pub peers: usize,
}

/// One tunnel of `POST /v1/peers/prepare`: the allocated address plus the card
/// the client builds its config from — the same card object `/v1/locations`
/// hands out, so the app parses one shape everywhere.
#[derive(Clone, Debug, Serialize)]
pub struct PreparedPeer {
    pub card_id: String,
    pub pool: String,
    pub proto: String,
    pub address: String,
    pub peer_id: Uuid,
    pub config: PoolCard,
}

/// A pool the batch did not provision, and the stable code that says why
/// (`peers_limit`, `card_kind_not_peer`, `proto_not_granted`, `db`, ...).
#[derive(Clone, Debug, Serialize)]
pub struct SkippedPool {
    pub card_id: String,
    pub reason: String,
}

fn prepare_body(prepared: &[PreparedPeer], skipped: &[SkippedPool]) -> serde_json::Value {
    serde_json::json!({ "prepared": prepared, "skipped": skipped })
}

fn device_view(row: &crate::postgres::peers::DeviceRow, live_peers: usize) -> DeviceView {
    DeviceView {
        id: row.id,
        device_uid: row.device_uid,
        device_name: row.device_name.clone(),
        support_tag: row.support_tag.clone(),
        created_at: row.created_at,
        rotated_at: row.rotated_at,
        peers: live_peers,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AccessView {
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

/// Protocol name as the app reads it: peer protocols go out in the snake_case
/// form of the contract (`amnezia_peer`), everything else keeps the config
/// spelling the rest of the panel already uses.
fn proto_wire_name(tag: &fcore::Tag) -> String {
    CardProto::from_tag(*tag)
        .map(|proto| proto.as_str().to_string())
        .unwrap_or_else(|| tag.to_string())
}

/// Handle of a location card: `<shard>:<pool>:<proto>`.
///
/// The app never reads the shard id — it echoes this handle back when it creates
/// a tunnel, and the api resolves it here. Keeping the mapping server-side means
/// the client payloads carry no env at all (see the app contract).
fn card_handle(env: &Env, pool: &str, proto: &str) -> String {
    format!("{}:{}:{}", env, pool, proto)
}

/// Why a `card_id` cannot name a peer tunnel. Split from the HTTP reply so the
/// decision (and its error code) is testable without a socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CardError {
    BadCardId,
    CardRequired,
    UnknownProto,
    BadEnv,
    /// The card exists but is built client-side: `POST /v1/peers` has nothing
    /// to allocate for it.
    CardKindNotPeer,
}

impl CardError {
    fn status(self) -> u16 {
        400
    }

    fn message(self) -> &'static str {
        match self {
            Self::BadCardId => "bad card_id",
            Self::CardRequired => "card_id (or env plus proto) is required",
            Self::UnknownProto => "unknown proto",
            Self::BadEnv => "bad env",
            Self::CardKindNotPeer => "card is not a peer card",
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::BadCardId => "bad_card_id",
            Self::CardRequired => "card_required",
            Self::UnknownProto => "unknown_proto",
            Self::BadEnv => "bad_env",
            Self::CardKindNotPeer => "card_kind_not_peer",
        }
    }

    fn reply(self) -> Box<dyn warp::Reply> {
        json_reply(
            self.status(),
            self.message(),
            serde_json::json!({ "error": self.code() }),
        )
    }
}

/// Resolve the card the client picked: `card_id` first, `env` + `proto` as the
/// manual/legacy form.
fn resolve_card_inner(
    card_id: &Option<String>,
    env: Option<&str>,
    proto: Option<&str>,
) -> Result<(Env, PeerProto, Option<String>), CardError> {
    let (env_raw, proto_raw, pool) = match card_id.as_deref() {
        Some(raw) => {
            let Some((head, proto_raw)) = raw.rsplit_once(':') else {
                return Err(CardError::BadCardId);
            };
            let Some((env_raw, pool)) = head.split_once(':') else {
                return Err(CardError::BadCardId);
            };
            (env_raw, proto_raw, (!pool.is_empty()).then(|| pool.to_string()))
        }
        None => {
            let (Some(env_raw), Some(proto_raw)) = (env, proto) else {
                return Err(CardError::CardRequired);
            };
            // The manual form names no pool, so the tunnel is not tied to a card.
            (env_raw, proto_raw, None)
        }
    };

    // `resolve_card` has to name the peer protocol of the card, so anything
    // that is not a peer is refused here instead of being tried as one.
    let proto = match CardProto::parse(proto_raw) {
        Some(CardProto::Peer(proto)) => proto,
        // hysteria2 and vless_xhttp_cdn are built client-side: no address, no
        // key, nothing this endpoint could allocate.
        Some(CardProto::Hysteria2 | CardProto::VlessXhttpCdn) => {
            return Err(CardError::CardKindNotPeer)
        }
        None => return Err(CardError::UnknownProto),
    };
    let env = Env::from_str(env_raw).map_err(|_| CardError::BadEnv)?;
    Ok((env, proto, pool))
}

/// The reply-producing wrapper of `resolve_card_inner`.
fn resolve_card(
    card_id: &Option<String>,
    env: Option<&str>,
    proto: Option<&str>,
) -> Result<(Env, PeerProto, Option<String>), Box<dyn warp::Reply>> {
    resolve_card_inner(card_id, env, proto).map_err(CardError::reply)
}

/// The pool `POST /v1/peers/prepare` allocates for this card, or `None` when
/// the card is built client-side: a `client_config` card (hysteria2,
/// `vless_xhttp_cdn`) is reported as `card_kind_not_peer` and never reaches
/// the peer path.
fn prepare_target(card: &PoolCard) -> Option<PoolTarget> {
    if card.kind != CardKind::Peer {
        return None;
    }
    match resolve_card_inner(&Some(card.card_id.clone()), None, None) {
        Ok((env, proto, pool)) => Some(PoolTarget {
            card_id: card.card_id.clone(),
            env,
            pool: pool.unwrap_or_else(|| "default".to_string()),
            proto,
        }),
        Err(_) => None,
    }
}

fn peer_view(row: &PeerRow) -> PeerView {
    let proto = PeerProto::from_tag(row.proto);
    PeerView {
        peer_id: row.id,
        device_id: row.device_id,
        proto: proto_wire_name(&row.proto),
        card_id: row
            .pool
            .as_deref()
            .zip(proto)
            .map(|(pool, proto)| card_handle(&row.env, pool, proto.as_str())),
        address: row.address.clone(),
        label: row.label.clone(),
        created_at: row.created_at,
        revoked_at: row.revoked_at,
        revoke_reason: row.revoke_reason.clone(),
    }
}

/// Display name of a plan. The single place where «lite is Super Duper» lives
/// is `crate::plans::title`; this is the local spelling of it.
fn plan_title(plan: Option<&str>) -> String {
    crate::plans::title(plan)
}

fn access_view(row: &AccessRow, used_bytes: i64) -> AccessView {
    AccessView {
        plan: row.plan.clone(),
        plan_title: plan_title(row.plan.as_deref()),
        protos: row.protos.iter().map(proto_wire_name).collect(),
        // The payment reference marker is our idempotency ledger, not a fact
        // about the purchase: the buyer sees «payment», never `pay:<uuid>`.
        sources: crate::plans::public_sources(&row.sources),
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

/// Endpoints one node contributes to a card: one per entry IP.
///
/// A node with several addresses (`[node] node_ips`) serves the same config on
/// all of them, and the app may dial any — reducing the card to the primary
/// address silently hides the rest of the node's entry points.
#[cfg(test)]
fn card_endpoints(hosts: Vec<String>, port: u16) -> Vec<String> {
    hosts
        .into_iter()
        .map(|host| format!("{}:{}", host, port))
        .collect()
}

/// Credentials the v1 plane already issues for a subscription: one uuid per
/// (env, protocol). The v2 card hands out this very uuid, so one credential
/// authenticates both planes and `access.rs` needs no change; a protocol the
/// subscription holds no live connection for gets no card instead of a
/// made-up one.
///
/// hysteria2 keeps its password on the connection (`Proto::Hysteria2.token`);
/// a VLESS uuid **is** the connection id, because v1 links are
/// `vless://<conn_id>@host`.
fn card_creds<C>(
    connections: &impl ConnectionStorageApiOperations<C>,
    subscription_id: &Uuid,
) -> HashMap<(Env, fcore::Tag), Uuid>
where
    C: ConnectionApiOperations + ConnectionBaseOperations + Clone + Send + Sync + 'static,
{
    connections
        .get_by_subscription_id(subscription_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, conn)| !conn.get_deleted())
        .filter_map(|(conn_id, conn)| {
            let tag = conn.get_proto().proto();
            if tag != fcore::Tag::Hysteria2 && tag != fcore::Tag::VlessXhttpCdn {
                return None;
            }
            Some(((conn.get_env(), tag), conn.get_token().unwrap_or(conn_id)))
        })
        .collect()
}

/// Credentials a card cannot be built without.
///
/// `hysteria2` and `vless_xhttp_cdn` cards carry the token of a *connection*:
/// the node terminates those proxies itself, so there is no peer address to
/// allocate. The v1 flow created that connection when a client asked for
/// locations; a v2 client only calls the peers endpoints, so a shard that starts
/// announcing hysteria2 had no credential to hand out and its card was dropped
/// in `pool_cards`. Minting it here keeps the v2 flow self-contained:
/// `create_connection_inner` still checks the grant, so a subscription without
/// the protocol gets nothing.
async fn ensure_granted_creds<N, C, S>(
    memory: &MemSync<N, C, S>,
    subscription_id: &Uuid,
    grants: &[AccessRow],
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
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + From<Subscription> + 'static,
    MemSync<N, C, S>: SyncOp<N, C, S>,
    S: From<Subscription>,
{
    let missing: Vec<(fcore::Env, fcore::Tag)> = {
        let mem = memory.memory.read().await;
        let have = card_creds(&mem.connections, subscription_id);

        let mut missing = Vec::new();

        for grant in grants {
            // Every client_config card this grant allows, not a hard-coded
            // pair: a shard that starts announcing a new proxy protocol gets
            // its credentials minted without touching this function.
            // `CardProto` stays the single place that decides which tags
            // become cards, so the loop cannot drift from `/v1/locations`.
            for tag in grant.protos.iter().copied().filter(|tag| {
                matches!(CardProto::from_tag(*tag), Some(card) if card.kind() == CardKind::ClientConfig)
            }) {
                if have.contains_key(&(grant.env.clone(), tag)) {
                    continue;
                }

                // A protocol nobody serves is not worth a connection row.
                let served = mem.nodes.iter_nodes().any(|(_, node)| {
                    node.env == grant.env
                        && node.status.is_serving()
                        && node.inbounds.contains_key(&tag)
                });

                if served {
                    missing.push((grant.env.clone(), tag));
                }
            }
        }

        missing
    };

    for (env, tag) in missing {
        let proto = match tag {
            fcore::Tag::Hysteria2 => fcore::Proto::Hysteria2 {
                token: Uuid::new_v4(),
            },
            other => fcore::Proto::Xray(other),
        };

        let conn: Connection = Connection::new(&env, Some(*subscription_id), proto, None);
        let conn_id = Uuid::new_v4();
        // Same shape the v1 path publishes: a `Vec<Message>`. The auth service
        // deserializes exactly that, and a bare slice fails its `CheckBytes`
        // with an `OutOfBounds` root — the token then never reaches auth memory
        // and every hysteria2 dial answers 403.
        let messages = vec![conn.as_create_message(&conn_id)];

        let bytes = match rkyv::to_bytes::<_, 1024>(&messages) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(subscription = %subscription_id, proto = ?tag, error = %e, "peers: cannot serialize a minted credential");
                continue;
            }
        };

        match SyncOp::add_conn(
            memory,
            &conn_id,
            conn,
            None,
            None,
            Some("peers".to_string()),
            false,
        )
        .await
        {
            Ok(_) => {
                // Token connections are broadcast on the auth topic: nodes read
                // their credentials from there, not from the env stream.
                let _ = memory
                    .publisher
                    .send_binary(&fcore::Topic::Auth, bytes.as_ref())
                    .await;

                tracing::info!(
                    subscription = %subscription_id,
                    proto = ?tag,
                    connection = %conn_id,
                    "peers: minted the credential of a granted protocol"
                );
            }
            Err(e) => tracing::warn!(
                subscription = %subscription_id,
                proto = ?tag,
                error = %e,
                "peers: cannot mint a credential"
            ),
        }
    }
}

/// Client parameters of a `vless_xhttp_cdn` card, built only from what the
/// node announces: an inbound without the xHTTP path and the CDN host it
/// terminates TLS for cannot produce a config, so it produces no card.
///
/// The announcement (`xraySettings.inbounds[]` on the node):
///
/// ```json
/// "streamSettings": {
///   "network": "xhttp",
///   "xhttpSettings": {
///     "path": "/downloads",
///     "mode": "packet-up",
///     "extra": { "padding": { "header": "X-Cache", "key": "_dc", "bytes": "100-1000" } }
///   },
///   "tlsSettings": { "serverName": "bs.frkn.org", "alpn": ["h2"], "certificates": [...] }
/// }
/// ```
///
/// `host` and `sni` are the announced TLS name (the CDN edge the client dials;
/// the node's own address stays the card `endpoint`), `padding` is passed
/// through verbatim from `extra.padding` only when it carries all three fields.
fn vless_xhttp_cdn_payload(
    inbound: &fcore::Inbound,
    uuid: &Uuid,
) -> Option<serde_json::Value> {
    let stream = inbound.stream_settings.as_ref()?;
    let transport = match stream.network {
        fcore::Network::Xhttp => "xhttp",
        // The tag says xhttp: an inbound announcing another network is not
        // the thing this card describes.
        _ => return None,
    };
    let xhttp = stream.xhttp_settings.as_ref()?;
    if xhttp.path.trim().is_empty() {
        return None;
    }
    let tls = stream.tls_settings.as_ref()?;
    let host = tls
        .server_name
        .as_ref()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())?;

    let mut payload = serde_json::Map::new();
    payload.insert("uuid".to_string(), uuid.to_string().into());
    payload.insert("transport".to_string(), transport.into());
    payload.insert("path".to_string(), xhttp.path.clone().into());
    payload.insert("host".to_string(), host.to_string().into());
    // The CDN terminates TLS under one name: `sni` is that same announcement.
    payload.insert("sni".to_string(), host.to_string().into());
    if let Some(mode) = xhttp.mode.as_ref().filter(|mode| !mode.trim().is_empty()) {
        payload.insert("mode".to_string(), mode.clone().into());
    }
    if let Some(alpn) = tls.alpn.as_ref().filter(|alpn| !alpn.is_empty()) {
        payload.insert("alpn".to_string(), serde_json::json!(alpn));
    }
    let padding = xhttp
        .extra
        .as_ref()
        .and_then(|extra| extra.get("padding"))
        .filter(|padding| {
            ["header", "key", "bytes"]
                .iter()
                .all(|field| padding.get(*field).is_some_and(|value| value.is_string()))
        });
    if let Some(padding) = padding {
        payload.insert("padding".to_string(), padding.clone());
    }
    Some(serde_json::Value::Object(payload))
}

/// Group the live nodes of one env into the cards the client shows.
///
/// A card is a `pool`: all its nodes share the client-facing parameters
/// (`pool_inbounds_conflict` enforces that at registration), so one config is
/// valid for any node of the card — only the endpoint differs.
///
/// `cred` is the subscription's existing credential for this protocol in this
/// env (hysteria2 token / VLESS uuid, see `card_creds`); it is only read by the
/// client_config branches and a missing one drops the card.
fn pool_cards<N: NodeStorageOperations>(
    nodes: &N,
    env: &Env,
    proto: CardProto,
    cred: Option<Uuid>,
) -> Vec<PoolCard> {
    let tag = proto.tag();
    /// One card while it is being filled: the pool's own attributes plus the
    /// nodes that carry them.
    struct Group {
        translation: Option<std::collections::BTreeMap<String, String>>,
        /// The country the pool declares (`[node] pool_country_*`), taken from
        /// the first node that has it: old binaries send nothing.
        pool_country: Option<(String, Option<String>)>,
        /// Country of the first node of the card, used when the pool declares
        /// none — a card with no flag at all would look broken in the list.
        first_node_country: Option<String>,
        nodes: Vec<(Uuid, PoolNode)>,
    }

    let mut grouped: HashMap<String, Group> = HashMap::new();

    for (uuid, node) in nodes.iter_nodes() {
        // Premium and self-hosted boxes keep their card while down; a dead core
        // node is hidden so nobody connects to an entry that is gone.
        if &node.env != env
            || (!node.status.is_serving() && !super::amnezia::keeps_card_while_offline(node))
        {
            continue;
        }
        let Some(inbound) = node.inbounds.get(&tag) else {
            continue;
        };

        let (server_public_key, mtu, dns, keepalive, obfuscation, payload): (
            String,
            u16,
            Vec<String>,
            Option<u16>,
            Option<serde_json::Value>,
            Option<serde_json::Value>,
        ) = match proto {
            CardProto::Peer(PeerProto::Amnezia) => {
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
                    None,
                )
            }
            CardProto::Peer(PeerProto::Wireguard) => {
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
                    None,
                )
            }
            CardProto::Hysteria2 => {
                let Some(h2) = inbound.h2.as_ref() else {
                    continue;
                };
                // No credential of this subscription in this env: a card with
                // an invented password would only fail at connect time.
                let Some(auth) = cred else {
                    continue;
                };
                // What the client dials: the node's entry IP, never the TLS
                // domain. The client keeps its own dial out of the tunnel by
                // excluding `hostName` as a /32 route (Xray.kt), and a domain
                // there excludes nothing: the QUIC handshake to the node then
                // loops back through the tunnel it is trying to build, which
                // shows up on the node as a stream to its own address refused.
                // The domain still travels in `sni`, which is what the
                // certificate is validated against.
                let server = node
                    .entry_ips()
                    .into_iter()
                    .next()
                    .or_else(|| {
                        (!h2.host.trim().is_empty()).then(|| h2.host.clone())
                    })
                    .unwrap_or_else(|| node.pool.clone().unwrap_or_default());
                let sni = h2
                    .sni
                    .clone()
                    .filter(|sni| !sni.trim().is_empty())
                    .unwrap_or_else(|| h2.host.clone());
                let payload = serde_json::json!({
                    "server": server,
                    "port": inbound.port,
                    "sni": sni,
                    "auth": auth.to_string(),
                    "insecure": h2.insecure,
                    "obfs": h2
                        .obfs
                        .as_ref()
                        .and_then(|o| serde_json::to_value(o).ok())
                        .unwrap_or(serde_json::Value::Null),
                });
                // A client-built config has no server key, address budget or
                // DNS to hand out: the node terminates the tunnel itself.
                (String::new(), 0, Vec::new(), None, None, Some(payload))
            }
            CardProto::VlessXhttpCdn => {
                // No credential of this subscription in this env: the uuid is
                // the v1 one, never invented here.
                let Some(uuid) = cred else {
                    continue;
                };
                let Some(payload) = vless_xhttp_cdn_payload(inbound, &uuid) else {
                    tracing::warn!(
                        "peers: vless_xhttp_cdn inbound on {} announces no path/host — node skipped",
                        node.hostname
                    );
                    continue;
                };
                // Same shape as hysteria2: the node terminates the tunnel, so
                // no server key, address budget or DNS travel with the card.
                (String::new(), 0, Vec::new(), None, None, Some(payload))
            }
        };

        // A numbered self-hosted shard holds one node per pool, and a node is
        // its own pool: the hostname. Falling back to `default` there would
        // collapse every node of the shard into a single card.
        let pool_name = node
            .pool
            .clone()
            .filter(|pool| !pool.trim().is_empty())
            .unwrap_or_else(|| {
                if node.env.selfhosted_number().is_some() {
                    node.hostname.clone()
                } else {
                    "default".to_string()
                }
            });
        let declared = node
            .pool_country_code
            .clone()
            .map(|code| (code, node.pool_country_name.clone()));
        let entry = grouped.entry(pool_name).or_insert_with(|| Group {
            translation: node.pool_translation.clone(),
            pool_country: None,
            first_node_country: Some(node.country.clone()),
            nodes: Vec::new(),
        });
        // Any node of the pool may be the one that declares the country.
        if entry.pool_country.is_none() {
            if let Some(declared) = declared {
                entry.pool_country = Some(declared);
            }
        }
        if entry.translation.is_none() {
            entry.translation = node.pool_translation.clone();
        }
        // A node may have several entry IPs (`[node] node_ips`): one config
        // stays valid for every address, so the card carries one entry per IP
        // instead of only the primary one — otherwise the client silently
        // loses the extra addresses of a node.
        for entry_point in node.endpoints_with_port(inbound.port) {
            let port = entry_point.port.unwrap_or(inbound.port);
            let endpoint = format!("{}:{}", entry_point.ip, port);
            entry.nodes.push((
                *uuid,
                PoolNode {
                    endpoint,
                    country: node.country.clone(),
                    server_public_key: server_public_key.clone(),
                    mtu,
                    dns: dns.clone(),
                    keepalive,
                    obfuscation: obfuscation.clone(),
                    payload: payload.clone(),
                    endpoints: vec![entry_point],
                },
            ));
        }
    }

    let mut cards: Vec<PoolCard> = grouped
        .into_iter()
        .map(|(pool, mut group)| {
            group.nodes.sort_by_key(|(uuid, _)| *uuid);
            let (country, country_name) = match group.pool_country {
                Some((code, name)) => (Some(code), name),
                None => (group.first_node_country, None),
            };
            PoolCard {
                card_id: card_handle(env, &pool, proto.as_str()),
                pool,
                pool_translation: group.translation,
                country,
                country_name,
                proto: proto.as_str().to_string(),
                kind: proto.kind(),
                nodes: group.nodes.into_iter().map(|(_, node)| node).collect(),
            }
        })
        .collect();
    cards.sort_by(|a, b| a.pool.cmp(&b.pool));
    cards
}

/// Cards of every protocol the subscription may actually use: the granted tags
/// (`subscription_access.protos`) intersected with what the env's serving nodes
/// announce. A tag outside the intersection produces no card at all, and tags
/// the api cannot card yet (VLESS, M3) are skipped.
fn cards_for_grants<N: NodeStorageOperations>(
    nodes: &N,
    grants: &[AccessRow],
    creds: &HashMap<(Env, fcore::Tag), Uuid>,
) -> Vec<PoolCard> {
    let mut out = Vec::new();
    for grant in grants {
        let mut seen: Vec<CardProto> = Vec::new();
        for tag in &grant.protos {
            let Some(proto) = CardProto::from_tag(*tag) else {
                continue;
            };
            if seen.contains(&proto) {
                continue;
            }
            seen.push(proto);
            // The subscription's own credential for this protocol, if the v1
            // plane already issued one; client_config branches drop the card
            // when it is absent.
            let cred = creds.get(&(grant.env.clone(), proto.tag())).copied();
            let cards = pool_cards(nodes, &grant.env, proto, cred);
            if cards.is_empty() {
                // One line per empty card: which env, which proto, and what
                // the nodes of that env announce — the answer to "почему
                // /v1/locations вернул pools: []".
                let announced: Vec<String> = nodes
                    .iter_nodes()
                    .filter(|(_, node)| &node.env == &grant.env)
                    .map(|(_, node)| {
                        format!(
                            "{:?} serving={} tags=[{}]",
                            node.status,
                            node.status.is_serving(),
                            node.inbounds.keys().map(|t| t.to_string()).collect::<Vec<_>>().join(",")
                        )
                    })
                    .collect();
                tracing::warn!(
                    "peers: no cards for env {:?} proto {} — nodes: [{}]",
                    grant.env,
                    proto.as_str(),
                    announced.join("; ")
                );
            }
            out.extend(cards);
        }
    }
    borrow_pool_names(&mut out, nodes);
    disambiguate_shared_pools(&mut out);
    out
}

/// Fill in a pool's display name from a sibling shard that declares one.
///
/// `pool_translation` lives in the node config (`[node] pool_translation`), and a
/// shard is provisioned by registering nodes — so a shard whose machines were set
/// up without that block sells `eu` while its twin sells `Netherlands`. The same
/// pool name is the same pool everywhere we run it, so the name is borrowed from
/// any node that declares it. Without this the client shows the raw `eu`, `ru`,
/// `us` for one shard and translated names for the other.
///
/// The borrowing is by pool name only and never overwrites a declared value: a
/// shard that names its own pool keeps its own name.
fn borrow_pool_names<N: NodeStorageOperations>(cards: &mut [PoolCard], nodes: &N) {
    let mut translations: std::collections::BTreeMap<&str, &std::collections::BTreeMap<String, String>> =
        std::collections::BTreeMap::new();
    let mut countries: std::collections::BTreeMap<&str, (&str, &str)> =
        std::collections::BTreeMap::new();

    // Deterministic donor: nodes are walked in storage order, which is stable
    // enough for a display name, but tie-break on hostname anyway so two
    // shards declaring different names for one pool cannot flip between calls.
    let mut donors: Vec<(String, String, Option<&std::collections::BTreeMap<String, String>>, Option<(&str, &str)>)> =
        nodes
            .iter_nodes()
            .filter_map(|(_, node)| {
                let pool = node.pool.as_deref()?.trim();
                if pool.is_empty() {
                    return None;
                }
                let translation = node
                    .pool_translation
                    .as_ref()
                    .filter(|t| !t.is_empty());
                let country = node
                    .pool_country_code
                    .as_deref()
                    .zip(node.pool_country_name.as_deref());
                if translation.is_none() && country.is_none() {
                    return None;
                }
                Some((pool.to_string(), node.hostname.clone(), translation, country))
            })
            .collect();
    donors.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));

    for (pool, _host, translation, country) in &donors {
        if let Some(translation) = translation {
            translations.entry(pool.as_str()).or_insert(translation);
        }
        if let Some(country) = country {
            countries.entry(pool.as_str()).or_insert(*country);
        }
    }

    for card in cards.iter_mut() {
        if card.pool_translation.as_ref().is_none_or(|t| t.is_empty()) {
            if let Some(translation) = translations.get(card.pool.as_str()) {
                card.pool_translation = Some((*translation).clone());
            }
        }
        if card.country_name.is_none() {
            if let Some((_, name)) = countries.get(card.pool.as_str()) {
                card.country_name = Some((*name).to_string());
            }
        }
    }
}

/// Number pools that share a name across shards: `Netherlands #1`, `Netherlands #2`.
///
/// A subscription may hold grants on several shards, and shards are provisioned
/// from the same template, so `eu` of `0001` and `eu` of `0002` carry the same
/// pool name and the same translation. The client shows one row per pool and
/// names the row by its translation, so two shards produced two rows the user
/// could not tell apart without opening each and reading an IP.
///
/// The suffix goes into `pool_translation`, never into `pool`: `pool` is part of
/// `card_id`, which the app echoes back to `POST /v1/peers`, and changing it here
/// would break the handle the client already holds. A card with no translation
/// gets one filled in with the numbered bare name, so the row still reads
/// `eu #1` / `eu #2` instead of two identical `eu`.
///
/// Only a name that really occurs in more than one shard is numbered, and only
/// from the second shard on: the first keeps the plain `Netherlands`. A
/// subscription on one shard therefore sees exactly what it saw before, and if
/// the first shard's grant is revoked the survivor loses its suffix again — the
/// collision is gone.
fn disambiguate_shared_pools(cards: &mut [PoolCard]) {
    // Identity of a card's name: the whole translation when there is one (the
    // client picks a language out of it), the raw pool name otherwise.
    fn name_key(card: &PoolCard) -> String {
        match card.pool_translation.as_ref().filter(|t| !t.is_empty()) {
            Some(translation) => {
                let mut pairs: Vec<String> = translation
                    .iter()
                    .map(|(lang, value)| format!("{}={}", lang, value))
                    .collect();
                pairs.sort();
                pairs.join("|")
            }
            None => card.pool.clone(),
        }
    }

    // `<shard>:<pool>:<proto>` — the shard is what makes two names different.
    fn shard_of(card: &PoolCard) -> &str {
        card.card_id.split(':').next().unwrap_or("")
    }

    let keys: Vec<String> = cards.iter().map(name_key).collect();
    let shards: Vec<String> = cards.iter().map(|card| shard_of(card).to_string()).collect();

    // Owned on purpose: the numbering below mutates `cards`, so nothing may keep
    // borrowing it.
    let mut shards_by_key: HashMap<String, std::collections::BTreeSet<String>> = HashMap::new();
    for (key, shard) in keys.iter().zip(shards.iter()) {
        shards_by_key
            .entry(key.clone())
            .or_default()
            .insert(shard.clone());
    }

    // Stable numbering: by shard id, then by pool, so `0001` is always #1 and the
    // number does not move between two requests.
    let mut order: Vec<usize> = (0..cards.len()).collect();
    order.sort_by(|a, b| {
        (shards[*a].as_str(), cards[*a].pool.as_str())
            .cmp(&(shards[*b].as_str(), cards[*b].pool.as_str()))
    });

    let mut seen: HashMap<String, usize> = HashMap::new();
    for index in order {
        let key = keys[index].clone();
        if shards_by_key.get(&key).map_or(0, |shards| shards.len()) < 2 {
            continue;
        }
        let counter = seen.entry(key).or_insert(0);
        *counter += 1;
        // The first shard keeps the plain name. A subscription that had only that
        // shard must not see "Netherlands" turn into "Netherlands #1" the day a
        // second grant appears — nothing about its own row changed.
        if *counter == 1 {
            continue;
        }
        let suffix = format!(" #{}", *counter);
        let card = &mut cards[index];
        match card.pool_translation.as_mut().filter(|t| !t.is_empty()) {
            Some(translation) => {
                for value in translation.values_mut() {
                    value.push_str(&suffix);
                }
            }
            None => {
                let numbered = format!("{}{}", card.pool, suffix);
                card.pool_translation = Some(std::collections::BTreeMap::from([
                    ("en".to_string(), numbered.clone()),
                    ("ru".to_string(), numbered.clone()),
                    ("uk".to_string(), numbered),
                ]));
            }
        }
    }
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

/// What each env's **serving** nodes announce, as `env -> tags`.
///
/// This is a shard's own protocol list: nothing in the config has to name it.
/// A shard that starts a pool with a new proxy protocol shows up here the
/// moment its node registers, and every consumer (grant filtering, credential
/// minting, card building) follows without a code change.
///
/// An empty map means the mesh has not loaded yet — callers must treat that as
/// "unknown", never as "nothing is served", or a restart would strip grants.
pub(crate) async fn announced_inbounds<N, C, S>(
    memory: &MemSync<N, C, S>,
) -> HashMap<String, HashSet<fcore::Tag>>
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
    let mem = memory.memory.read().await;
    let mut out: HashMap<String, HashSet<fcore::Tag>> = HashMap::new();
    for (_, node) in mem.nodes.iter_nodes() {
        if !node.status.is_serving() {
            continue;
        }
        let entry = out.entry(node.env.to_string()).or_default();
        for tag in node.inbounds.keys() {
            entry.insert(*tag);
        }
    }
    out
}

/// Shards of a plan that can take a subscription right now.
///
/// Only envs with at least one **serving** node announcing a peer interface are
/// returned: assigning into an env whose machines are gone hands out a tunnel
/// nobody serves. The limit is the `env_capacity` override when set, otherwise
/// the **address budget** of the env (blocks minus the 10% reserve): peers are
/// the resource that actually runs out, not subscription rows.
pub(crate) async fn live_shards<N, C, S>(memory: &MemSync<N, C, S>, plan: &str) -> Vec<(String, i64)>
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
    let (envs_with_nodes, address_limits): (HashMap<String, i64>, HashMap<String, i64>) = {
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
        let mut limits: HashMap<String, i64> = HashMap::new();
        for env_name in counts.keys() {
            let Ok(env) = env_name.parse::<Env>() else {
                continue;
            };
            let budget = alloc::allocatable(&mem.peers_state.block_masks(&env));
            limits.insert(env_name.clone(), budget as i64);
        }
        (counts, limits)
    };

    let overrides = memory
        .db
        .peers()
        .plan_shards(plan)
        .await
        .unwrap_or_default();

    let mut shards: Vec<(String, i64)> = Vec::new();
    for (env, _serving_nodes) in envs_with_nodes {
        // `env_capacity` is the mapping plan → shard: an env that is not
        // declared for THIS plan must never take its users, even when it has
        // live nodes (otherwise a `super-duper` shard serves `standard`
        // subscribers just because nobody disabled it).
        let Some(row) = overrides.iter().find(|(e, _, _)| e == &env) else {
            continue;
        };
        let (_, max, enabled) = row;
        if !*enabled {
            continue; // switched off by the admin
        }
        let budget = *address_limits.get(&env).unwrap_or(&0);
        if budget == 0 {
            continue; // no address space: a peer here could never be created
        }
        let limit = match max {
            Some(max) => *max,
            None => budget,
        };
        shards.push((env, limit));
    }
    // Deterministic order: least-loaded ties break on the env name.
    shards.sort_by(|a, b| a.0.cmp(&b.0));
    shards
}

// ---------------------------------------------------------------- publish

/// Push one peer change to every node of the pool the peer belongs to.
///
/// The stream is pool-scoped: the same device key may hold one address per
/// pool, and a node of another pool must not receive (nor apply) it.
///
/// A failure is logged and swallowed: the peer already exists in the database,
/// and the node plane heals on its next start (authoritative `peers-init`),
/// so a transient bus hiccup must not fail the user's request.
pub(crate) async fn publish_peer<N, C, S>(
    memory: &MemSync<N, C, S>,
    action: fcore::Action,
    peer: &PeerRow,
)
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

    // An account-owned shard is not a shard we run: its stream must never be
    // readable by subscribing to `peers-update-<env>-<pool>`, because that
    // string is derived from what the installer prints. Every node of the shard
    // gets the authoritative set on its own `peers-init-<uuid>` topic instead —
    // unicast, and the set is selected here from the *stored* env, never from a
    // request.
    if peer.env.is_personal() {
        let targets: Vec<(Uuid, Option<String>, Vec<fcore::Tag>)> = {
            let mem = memory.memory.read().await;
            mem.nodes
                .get_by_env(&peer.env)
                .map(|nodes| {
                    nodes
                        .iter()
                        .map(|node| {
                            (
                                node.uuid,
                                node.pool.clone(),
                                node.inbounds
                                    .keys()
                                    .copied()
                                    .filter(|tag| tag.is_peer())
                                    .collect::<Vec<_>>(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        for (node_uuid, pool, tags) in targets {
            if tags.is_empty() {
                continue;
            }
            publish_peers_init(memory, node_uuid, &peer.env, pool.as_deref(), &tags).await;
        }
        return;
    }

    let topic = fcore::Topic::PeersUpdates(peer.env.clone(), peer.pool.clone());
    if let Err(e) = memory.publisher.send_binary(&topic, bytes.as_ref()).await {
        tracing::error!("peers: publish {} failed: {}", topic, e);
    }
}

/// The live peer rows one node must hold: its own pool, or the whole env when
/// the node declares no pool (pre-pool config).
///
/// A node programs `(public_key, address)` pairs, and the pool decides which of
/// them are its business. Sending the env's whole set is what put the EU
/// address of a US device key on the US interface, so the tunnel came up with
/// no traffic: the kernel only accepted an address the client did not have.
pub(crate) fn peers_for_node<'a>(
    rows: &'a [PeerRow],
    env: &Env,
    tags: &[fcore::Tag],
    pool: Option<&str>,
) -> Vec<&'a PeerRow> {
    rows.iter()
        // Defence in depth for own shards: the set is chosen from the env the
        // api stored for the node, so a row of another account can never ride
        // along even if a caller hands us a wider slice.
        .filter(|row| &row.env == env)
        .filter(|row| tags.contains(&row.proto))
        .filter(|row| pool.map_or(true, |pool| row.pool.as_deref() == Some(pool)))
        .collect()
}

/// Publish the authoritative peer set of one node's `(env, pool)` to it.
///
/// Called when a node registers: the node subscribes to `peers-init-<uuid>`
/// at start, and without this push it would sit empty until somebody created
/// or deleted a tunnel. An empty set is meaningful — it tells the node to
/// prune everything it still has.
pub async fn publish_peers_init<N, C, S>(
    memory: &MemSync<N, C, S>,
    node_uuid: Uuid,
    env: &Env,
    pool: Option<&str>,
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

    let rows = peers_for_node(&rows, env, tags, pool);
    let messages: Vec<fcore::PeerMessage> = rows
        .into_iter()
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
            "peers: sent {} tunnels of {} pool {:?} to node {}",
            count,
            env,
            pool,
            node_uuid
        ),
        Err(e) => tracing::error!("peers: init publish {} failed: {}", topic, e),
    }
}

// ---------------------------------------------------------------- handlers

/// Find or create the device row of an installation.
///
/// Shared by `POST /v1/peers` (one tunnel) and `POST /v1/peers/prepare` (every
/// pool of the grant): the device is created lazily on its first tunnel, counts
/// against the subscription's device cap only when it is new, and is written
/// through to memory so the per-device caps see it immediately.
async fn ensure_device<N, C, S>(
    memory: &MemSync<N, C, S>,
    subscription_id: Uuid,
    device_uid: Uuid,
    device_name: Option<&str>,
    platform: Option<&str>,
    public_key: Option<&str>,
    default_limits: Limits,
) -> Result<crate::postgres::peers::DeviceRow, Box<dyn warp::Reply>>
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
    let mut device = match db.device_by_uid(subscription_id, device_uid).await {
        Ok(device) => device,
        Err(e) => return Err(db_error("device_by_uid", e)),
    };

    if device.is_none() {
        // A new installation counts against the subscription's device cap; an
        // existing one creating another tunnel must not be counted twice.
        let devices_live = {
            let mem = memory.memory.read().await;
            mem.peers_state.devices.of_sub(&subscription_id).len() as i64
        };
        if let Err(refusal) = default_limits.check_devices(devices_live) {
            return Err(refusal_reply(refusal));
        }
        let Some(key) = public_key else {
            return Err(refusal_reply(Refusal::BadPublicKey));
        };
        let mut created = false;
        for _ in 0..5 {
            let tag = support_tag();
            match db
                .insert_device(
                    subscription_id,
                    device_uid,
                    device_name,
                    key,
                    platform,
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
                    if let Ok(Some(existing)) = db.device_by_uid(subscription_id, device_uid).await {
                        device = Some(existing);
                        break;
                    }
                }
                Err(e) => return Err(db_error("insert_device", e)),
            }
        }
        if device.is_none() {
            if !created {
                return Err(json_reply(
                    409,
                    "support tag collision",
                    serde_json::json!({ "error": "support_tag_taken" }),
                ));
            }
            match db.device_by_uid(subscription_id, device_uid).await {
                Ok(existing) => device = existing,
                Err(e) => return Err(db_error("device_by_uid", e)),
            }
        }
    }

    let device = match device {
        Some(device) => device,
        None => return Err(refusal_reply(Refusal::DeviceNotFound)),
    };

    // Write-through of the device row: the per-device peer cap counts tunnels
    // through `devices.public_key`, so a device this request just created must
    // be visible in memory — otherwise it would look like it has no tunnels at
    // all (and `devices_per_subscription` would not see it either) until the
    // next restart.
    memory
        .memory
        .write()
        .await
        .peers_state
        .devices
        .insert(device.clone());

    Ok(device)
}

/// POST /v1/peers
pub async fn post_peers_handler<N, C, S>(
    req: PeerCreateRequest,
    memory: MemSync<N, C, S>,
    default_limits: Limits,
    auto_pool: Option<fcore::IpAddrMask>,
    auto_prefix: u8,
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
    let (env, proto, pool) =
        match resolve_card(&req.card_id, req.env.as_deref(), req.proto.as_deref()) {
            Ok(card) => card,
            Err(reply) => return reply,
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

    let device = match ensure_device(
        &memory,
        req.subscription_id,
        req.device_uid,
        req.device_name.as_deref(),
        None,
        req.public_key.as_deref(),
        default_limits,
    )
    .await
    {
        Ok(device) => device,
        Err(reply) => return reply,
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

    let public_key = req
        .public_key
        .clone()
        .unwrap_or_else(|| device.public_key.clone());

    // Idempotency first: the same key in the same env **and pool** is the same
    // tunnel, and a re-create must not be refused by a cap it already occupies.
    // A client retrying after a timeout lands here and gets its peer back, even
    // at the limit. The pool matters: one device may hold a tunnel in every
    // pool of the shard, and the card the caller named decides which one this
    // request means.
    let existing = {
        let mem = memory.memory.read().await;
        mem.peers_state
            .peers
            .live_by_key_in(&env, pool.as_deref(), &public_key)
            .cloned()
    };
    match existing {
        Some(existing) if existing.subscription_id == req.subscription_id => {
            let cards = {
                let mem = memory.memory.read().await;
                // The caller's card is a peer card (hysteria2 is refused in
                // `resolve_card`), so the reply lists peer cards only.
                pool_cards(&mem.nodes, &env, CardProto::Peer(proto), None)
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

    let (per_device, per_subscription) = {
        let mem = memory.memory.read().await;
        mem.peers_state.limits_for(&req.subscription_id)
    };
    let limits = Limits::resolve(default_limits, per_device, per_subscription);
    // The peer cap is per physical device **per pool**: three tunnels in one
    // pool (a router keeps two up for failover), and every pool has its own
    // three. Counting across all pools refused the fourth pool's tunnel while
    // each pool still had a free slot.
    let (device_live, sub_live) = {
        let mem = memory.memory.read().await;
        (
            mem.peers_state
                .peers_of_device_key_in(&device.public_key, &env, pool.as_deref().unwrap_or("")),
            mem.peers_state
                .peers
                .counts(&req.subscription_id, &device.id)
                .1 as i64,
        )
    };
    if let Err(refusal) = limits.check_device_peers(device_live) {
        return refusal_reply(refusal);
    }
    if let Err(refusal) = limits.check_subscription(sub_live) {
        return refusal_reply(refusal);
    }

    let (mut blocks, mut used) = {
        let mem = memory.memory.read().await;
        (
            mem.peers_state.block_masks_for(&env, pool.as_deref()),
            mem.peers_state.peers.used_in(&env),
        )
    };
    if blocks.is_empty() {
        // Shards whose number is not known in advance — premium slots, a
        // customer's own machine — cannot be listed in the api config, so their
        // address space is carved out of the reserved pool on first use.
        let Some(pool) = auto_pool.as_ref() else {
            return refusal_reply(Refusal::EnvNotServed);
        };
        match db.allocate_env_block(&env, pool, auto_prefix).await {
            Ok(Some(block)) => {
                tracing::info!("peers: shard {} got {} from the auto pool", env, block);
                memory
                    .memory
                    .write()
                    .await
                    .peers_state
                    .env_networks
                    .entry(env.clone())
                    .or_default()
                    .push(block.to_string());
                blocks = vec![block];
            }
            Ok(None) => {
                tracing::error!(
                    "peers: the auto pool is exhausted, shard {} cannot hand out an address",
                    env
                );
                return refusal_reply(Refusal::EnvNotServed);
            }
            Err(e) => return db_error("allocate_env_block", e),
        }
    }
    // Keep the reserve of the env's blocks free: an address freed by a revoke
    // goes to the next request, but a burst of re-issues (card switch,
    // reinstall) must not eat the last host of the block.
    let budget = alloc::allocatable(&blocks);
    if used.len() as u64 >= budget {
        tracing::warn!(
            "peers: env {} is at its address budget ({}/{}), refusing a new peer",
            env,
            used.len(),
            budget
        );
        return refusal_reply(Refusal::NoFreeAddress);
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
                pool.as_deref(),
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
        pool_cards(&mem.nodes, &env, CardProto::Peer(proto), None)
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

/// `POST /v1/peers/prepare`
///
/// Provisions one tunnel per peer pool of the subscription's grant in a single
/// call, so the client can store every config at device login / subscription
/// import and run its speed test without connecting first.
///
/// One pool never fails the call: a full pool, a refused protocol or a database
/// hiccup lands in `skipped` with the code `POST /v1/peers` would have answered
/// for it, and the other pools are still provisioned. Every insert is its own
/// statement, so there is no batch-wide transaction to roll back.
pub async fn post_peers_prepare_handler<N, C, S>(
    req: PeerPrepareRequest,
    memory: MemSync<N, C, S>,
    default_limits: Limits,
    auto_pool: Option<fcore::IpAddrMask>,
    auto_prefix: u8,
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
    MemSync<N, C, S>: SyncOp<N, C, S>,
    S: From<Subscription>,
{
    if !peers::is_valid_public_key(&req.public_key) {
        return refusal_reply(Refusal::BadPublicKey);
    }

    {
        let mem = memory.memory.read().await;
        match mem.subscriptions.find_by_id(&req.subscription_id) {
            Some(sub) if !sub.is_deleted() => {}
            _ => return refusal_reply(Refusal::NoSubscription),
        }
    }

    let device = match ensure_device(
        &memory,
        req.subscription_id,
        req.device_uid,
        req.device_name.as_deref(),
        req.platform.as_deref(),
        Some(&req.public_key),
        default_limits,
    )
    .await
    {
        Ok(device) => device,
        Err(reply) => return reply,
    };

    let db = memory.db.peers();

    // Grants of this subscription, their peer cards and the effective limits:
    // the same sources the single create reads, so a pool cannot be allowed
    // here and refused there.
    let grants: Vec<AccessRow> = {
        let mem = memory.memory.read().await;
        mem.peers_state
            .access
            .of_sub(&req.subscription_id)
            .into_iter()
            .cloned()
            .collect()
    };

    // `hysteria2`/`vless` cards are built from a connection token that a v2
    // client never creates itself: mint what the grant allows before reading
    // the credentials, otherwise those cards are dropped in `pool_cards`.
    ensure_granted_creds(&memory, &req.subscription_id, &grants).await;

    let (cards, limits) = {
        let mem = memory.memory.read().await;
        let creds = card_creds(&mem.connections, &req.subscription_id);
        let cards = cards_for_grants(&mem.nodes, &grants, &creds);
        let (per_device, per_subscription) = mem.peers_state.limits_for(&req.subscription_id);
        (
            cards,
            Limits::resolve(default_limits, per_device, per_subscription),
        )
    };
    let grants_as_grant: Vec<Grant> = grants.iter().map(grant_of).collect();

    // The optional filter names `<shard>:<pool>`: the same handle minus the
    // protocol. A requested handle that names no card is reported, otherwise a
    // typo would silently prepare nothing.
    let requested: Option<HashSet<String>> = req.pools.as_ref().map(|pools| {
        pools
            .iter()
            .map(|pool| pool.trim().to_string())
            .filter(|pool| !pool.is_empty())
            .collect()
    });
    let mut matched: HashSet<String> = HashSet::new();
    let mut targets: Vec<PoolTarget> = Vec::new();
    let mut non_peer: Vec<(String, String)> = Vec::new();
    let mut card_by_id: HashMap<String, PoolCard> = HashMap::new();

    for card in cards {
        let handle = card
            .card_id
            .rsplit_once(':')
            .map(|(head, _proto)| head.to_string())
            .unwrap_or_else(|| card.card_id.clone());
        if let Some(wanted) = &requested {
            if !wanted.contains(&handle) {
                continue;
            }
            matched.insert(handle);
        }
        card_by_id.insert(card.card_id.clone(), card.clone());
        match prepare_target(&card) {
            Some(target) => targets.push(target),
            // A `client_config` card (hysteria2, vless_xhttp_cdn) has no peer
            // to allocate: the app builds it from `nodes[].payload` and must
            // not be told to dial `POST /v1/peers`. An unparsable protocol is
            // reported the same way instead of failing the batch.
            None => non_peer.push((card.card_id.clone(), card.proto.clone())),
        }
    }

    let mut skipped: Vec<SkippedPool> = Vec::new();
    if let Some(wanted) = &requested {
        let mut unknown: Vec<&String> = wanted
            .iter()
            .filter(|handle| !matched.contains(*handle))
            .collect();
        unknown.sort();
        for handle in unknown {
            skipped.push(SkippedPool {
                card_id: handle.clone(),
                reason: "unknown_pool".to_string(),
            });
        }
    }

    if targets.is_empty() {
        return json_reply(200, "peers prepared", prepare_body(&[], &skipped));
    }

    // Per-env prerequisites: address space (the auto pool carves a block for a
    // shard whose number is not known in advance) and the traffic meter.
    let mut envs: Vec<Env> = targets.iter().map(|target| target.env.clone()).collect();
    envs.sort_by(|a, b| a.to_string().cmp(&b.to_string()));
    envs.dedup();

    let mut env_errors: HashMap<Env, &'static str> = HashMap::new();
    let mut quota_spent: HashSet<Env> = HashSet::new();
    for env in &envs {
        let blocks = {
            let mem = memory.memory.read().await;
            mem.peers_state.block_masks(env)
        };
        if blocks.is_empty() {
            match auto_pool.as_ref() {
                Some(pool) => match db.allocate_env_block(env, pool, auto_prefix).await {
                    Ok(Some(block)) => {
                        tracing::info!(
                            "peers: prepare got {} for shard {} from the auto pool",
                            block,
                            env
                        );
                        memory
                            .memory
                            .write()
                            .await
                            .peers_state
                            .env_networks
                            .entry(env.clone())
                            .or_default()
                            .push(block.to_string());
                    }
                    Ok(None) => {
                        tracing::error!(
                            "peers: the auto pool is exhausted, shard {} cannot hand out an address",
                            env
                        );
                        env_errors.insert(env.clone(), "env_not_served");
                    }
                    Err(e) => {
                        tracing::error!("peers: prepare auto block for {} failed: {}", env, e);
                        env_errors.insert(env.clone(), "db");
                    }
                },
                None => {
                    env_errors.insert(env.clone(), "env_not_served");
                }
            }
        }

        if let Some(grant) = grants_as_grant.iter().find(|grant| &grant.env == env) {
            if let Some(limit) = grant.traffic_limit {
                match db.used_bytes(req.subscription_id, env).await {
                    Ok(used) if used >= limit => {
                        quota_spent.insert(env.clone());
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::error!("peers: prepare used_bytes for {} failed: {}", env, e);
                        env_errors.insert(env.clone(), "db");
                    }
                }
            }
        }
    }

    let blocks: HashMap<Env, Vec<fcore::IpAddrMask>> = {
        let mem = memory.memory.read().await;
        envs.iter()
            .map(|env| (env.clone(), mem.peers_state.block_masks(env)))
            .collect()
    };

    let decisions = {
        let mem = memory.memory.read().await;
        let batch = Batch {
            state: &mem.peers_state,
            subscription_id: req.subscription_id,
            device_key: &device.public_key,
            peer_key: &req.public_key,
            limits,
            now: Utc::now(),
            quota_spent: &quota_spent,
            blocks: &blocks,
            env_errors: &env_errors,
        };
        prepare::plan(&batch, &grants_as_grant, &targets, &non_peer)
    };

    let mut prepared: Vec<PreparedPeer> = Vec::new();
    for decision in decisions {
        match decision {
            Decision::Skip { card_id, reason } => skipped.push(SkippedPool {
                card_id,
                reason: reason.to_string(),
            }),
            Decision::Existing { target, peer } => {
                let Some(config) = card_by_id.get(&target.card_id) else {
                    skipped.push(SkippedPool {
                        card_id: target.card_id,
                        reason: "card_missing".to_string(),
                    });
                    continue;
                };
                prepared.push(PreparedPeer {
                    card_id: target.card_id,
                    pool: target.pool,
                    proto: target.proto.as_str().to_string(),
                    address: peer.address,
                    peer_id: peer.id,
                    config: config.clone(),
                });
            }
            Decision::Create { target, address } => {
                let Some(config) = card_by_id.get(&target.card_id) else {
                    skipped.push(SkippedPool {
                        card_id: target.card_id,
                        reason: "card_missing".to_string(),
                    });
                    continue;
                };

                // One insert per pool, autocommitted: a failure here is this
                // pool's failure only. A conflict (another api instance took
                // the address or the key) retries with the next free address,
                // like the single create does.
                let (blocks_env, mut used) = {
                    let mem = memory.memory.read().await;
                    (
                        mem.peers_state.block_masks_for(&target.env, Some(target.pool.as_str())),
                        mem.peers_state.peers.used_in(&target.env),
                    )
                };
                let mut next = address;
                let mut reason = "no_free_address";
                let mut created: Option<PeerRow> = None;
                for attempt in 0..3 {
                    match db
                        .insert_peer(
                            device.id,
                            req.subscription_id,
                            &target.env,
                            target.proto.tag(),
                            &req.public_key,
                            next,
                            Some(target.pool.as_str()),
                            None,
                        )
                        .await
                    {
                        Ok(Some(row)) => {
                            created = Some(row);
                            break;
                        }
                        Ok(None) => {
                            tracing::info!(
                                "peers: prepare insert conflict (attempt {}) for {} in {}:{}",
                                attempt,
                                req.public_key,
                                target.env,
                                target.pool
                            );
                            used.insert(next);
                            match alloc::first_free(&blocks_env, &used) {
                                Some(free) => next = free,
                                None => break,
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                "peers: prepare insert_peer for {}:{} failed: {}",
                                target.env,
                                target.pool,
                                e
                            );
                            reason = "db";
                            break;
                        }
                    }
                }
                let Some(peer) = created else {
                    skipped.push(SkippedPool {
                        card_id: target.card_id,
                        reason: reason.to_string(),
                    });
                    continue;
                };

                memory
                    .memory
                    .write()
                    .await
                    .peers_state
                    .peers
                    .insert(peer.clone());
                publish_peer(&memory, fcore::Action::Create, &peer).await;

                prepared.push(PreparedPeer {
                    card_id: target.card_id,
                    pool: target.pool,
                    proto: target.proto.as_str().to_string(),
                    address: peer.address,
                    peer_id: peer.id,
                    config: config.clone(),
                });
            }
        }
    }

    json_reply(200, "peers prepared", prepare_body(&prepared, &skipped))
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
            match db.revoke_peer(peer_id, peers::REVOKE_USER).await {
                Ok(true) => {
                    memory
                        .memory
                        .write()
                        .await
                        .peers_state
                        .peers
                        .remove_live(peer_id);
                    publish_peer(&memory, fcore::Action::Delete, &peer).await;
                    json_reply(200, "peer revoked", serde_json::json!({ "status": "ok" }))
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
                .map(|(row, live)| device_view(&row, live.max(0) as usize))
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
                Ok(true) => {
                    // Answer with the row as it now stands, live peers
                    // included: the app re-renders the device list from it.
                    let renamed = match db.device_by_id(device_id).await {
                        Ok(Some(row)) => row,
                        _ => device,
                    };
                    let live = {
                        let mem = memory.memory.read().await;
                        mem.peers_state.peers.live_of_device(&device_id).len()
                    };
                    json_reply(
                        200,
                        "renamed",
                        serde_json::json!({ "device": device_view(&renamed, live) }),
                    )
                }
                Ok(false) => refusal_reply(Refusal::DeviceNotFound),
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
                if db.revoke_peer(peer.id, peers::REVOKE_USER).await.is_ok() {
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
                Ok(_) => {
                    // Postgres is the source of truth, but `POST /v1/peers`
                    // counts a subscription's devices from the in-memory
                    // registry (`devices.of_sub`). Leaving the row there keeps
                    // the user at their device cap after deleting one: the
                    // replacement install answers `devices_limit` until the api
                    // restarts. `reap_stale_devices` already drops its own
                    // rows; the explicit delete must do the same.
                    memory
                        .memory
                        .write()
                        .await
                        .peers_state
                        .devices
                        .remove(&device_id);
                    json_reply(200, "device deleted", serde_json::json!({ "status": "ok" }))
                }
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
    MemSync<N, C, S>: SyncOp<N, C, S>,
    S: From<Subscription>,
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

    // Same reason as `post_peers_prepare_handler`: a `hysteria2` card is built
    // from a connection token, and a v2 client never creates one itself.
    ensure_granted_creds(&memory, &subscription_id, &grants).await;

    let cards = {
        let mem = memory.memory.read().await;
        let creds = card_creds(&mem.connections, &subscription_id);
        cards_for_grants(&mem.nodes, &grants, &creds)
    };

    json_reply(
        200,
        "ok",
        // `contract` lets the app tell a v2 answer (kind/payload cards) from a
        // v1 one without sniffing the cards themselves.
        serde_json::json!({ "contract": 2, "access": access, "pools": cards }),
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
        match peers::tag_from_wire(name) {
            Some(tag) => protos.push(tag),
            None => {
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
        let shards = live_shards(&memory, &plan).await;
        // Same reason as `key.rs`: filter the requested protocols down to what
        // the chosen shard actually announces.
        let announced = announced_inbounds(&memory).await;
        return match db
            .assign_grant(
                req.subscription_id,
                &plan,
                &protos,
                &source,
                req.expires_at,
                req.traffic_limit,
                &shards,
                &announced,
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
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static + From<Subscription>,
{
    let env = match Env::from_str(&req.env) {
        Ok(env) => env,
        Err(_) => return json_reply(400, "bad env", serde_json::json!({ "error": "bad_env" })),
    };

    match revoke_env_access(
        &memory,
        req.subscription_id,
        &env,
        peers::REVOKE_SUBSCRIPTION_ENDED,
    )
    .await
    {
        Ok(Some((peers_revoked, conns_revoked))) => json_reply(
            200,
            "grant revoked",
            serde_json::json!({ "peers_revoked": peers_revoked, "conns_revoked": conns_revoked }),
        ),
        Ok(None) => json_reply(
            404,
            "no live grant",
            serde_json::json!({ "error": "no_grant" }),
        ),
        Err(e) => json_reply(
            500,
            "revoke failed",
            serde_json::json!({ "error": "db", "message": e }),
        ),
    }
}

/// Take a grant away along with everything issued from it: the peers programmed
/// on the nodes and the v1 connections of that env.
///
/// Shared by `DELETE /v1/access` and the expiry task, so an expired term and a
/// refund end the same way. `Ok(None)` means the grant was already gone.
pub(crate) async fn revoke_env_access<N, C, S>(
    memory: &MemSync<N, C, S>,
    subscription_id: Uuid,
    env: &Env,
    reason: &str,
) -> Result<Option<(i64, usize)>, String>
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
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static + From<Subscription>,
{
    let db = memory.db.peers();
    let had_grant = db
        .revoke_grant(subscription_id, env, reason)
        .await
        .map_err(|e| e.to_string())?;
    if !had_grant {
        return Ok(None);
    }

    memory
        .memory
        .write()
        .await
        .peers_state
        .access
        .remove(&subscription_id, env);

    let live: Vec<PeerRow> = {
        let mem = memory.memory.read().await;
        mem.peers_state
            .peers
            .live_of_sub(&subscription_id)
            .into_iter()
            .filter(|peer| &peer.env == env)
            .cloned()
            .collect()
    };
    let peers_revoked = db
        .revoke_env_peers(subscription_id, env, reason)
        .await
        .unwrap_or(0);
    memory
        .memory
        .write()
        .await
        .peers_state
        .peers
        .remove_env_of_sub(&subscription_id, env);
    for peer in &live {
        publish_peer(memory, fcore::Action::Delete, peer).await;
    }

    // The v1 connections issued for this env go with the right: leaving them
    // alive until their own term is what a refunded customer would exploit. The
    // subscription's own env is skipped — it is a self-hosted server, not
    // something we granted.
    let conns: Vec<(Uuid, C)> = {
        let mem = memory.memory.read().await;
        let scope_env = mem
            .subscriptions
            .find_by_id(&subscription_id)
            .and_then(|sub| sub.scope_env().cloned());
        if scope_env.as_ref() == Some(env) {
            Vec::new()
        } else {
            mem.connections
                .get_by_subscription_id(&subscription_id)
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, conn)| !conn.get_deleted() && conn.get_env() == *env)
                .collect()
        }
    };
    let mut conns_revoked = 0usize;
    for (conn_id, conn) in conns {
        match SyncOp::delete_connection(
            memory,
            &conn_id,
            &conn.into(),
            Some(DELETE_REASON_GRANT_CHANGED),
        )
        .await
        {
            Ok(_) => conns_revoked += 1,
            Err(e) => tracing::error!(
                "revoke_env_access: cannot delete connection {}: {:?}",
                conn_id,
                e
            ),
        }
    }

    Ok(Some((peers_revoked, conns_revoked)))
}

/// A successful date extension — payment-gw `PUT /subscription`, support days,
/// Apple IAP, key renewal — must reach the grants, not just the subscription
/// row: each grant carries its own `expires_at`, and `expire_grants` closes it
/// on that clock. This extends the live ones and revives the ones the clock
/// (`expired`) or the meter (`quota`) took away.
///
/// A revoked grant stays on the same env, pool and protos, so the client lands
/// on the same shard. Explicit revocations (`user`, `subscription_ended`) are
/// not touched, and a subscription without grants is a no-op (a v1 user gets
/// nothing new here — creating grants is the activation path's decision).
///
/// Peers revoked by that same expiry are *not* un-revoked: the api already
/// told the nodes to drop them, so the client gets a fresh tunnel from the
/// next `POST /v1/peers` instead of a row nobody programmed.
pub(crate) async fn renew_grant_access<N, C, S>(
    memory: &MemSync<N, C, S>,
    subscription_id: Uuid,
    expires_at: DateTime<Utc>,
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
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static + From<Subscription>,
{
    match memory
        .db
        .peers()
        .renew_grants(subscription_id, expires_at)
        .await
    {
        Ok(rows) if rows.is_empty() => {}
        Ok(rows) => {
            let mut state = memory.memory.write().await;
            for renewed in rows {
                let env = renewed.row.env.clone();
                if renewed.revived {
                    tracing::info!(
                        "renewal: grant revived for subscription {} env {} until {} (was revoked: {}), peers are reissued on the next POST /v1/peers",
                        subscription_id,
                        env,
                        expires_at,
                        renewed.revoked_reason.as_deref().unwrap_or("no reason, term was over")
                    );
                } else {
                    tracing::info!(
                        "renewal: grant extended for subscription {} env {} until {}",
                        subscription_id,
                        env,
                        expires_at
                    );
                }
                state.peers_state.access.insert(subscription_id, renewed.row);
            }
        }
        Err(err) => tracing::error!(
            "renewal: cannot renew grants of {}: {}",
            subscription_id,
            err
        ),
    }
}

/// `GET /cabinet/<subscription_id>` — плоский JSON для ЛК (без конверта, как
/// `/subscription/<id>` и `/sub`): на какой схеме подписка и что показывать.
///
/// Схема решается здесь, а не в браузере: гранты — новый мир, v1-коннекты —
/// старый, и во время переезда у человека может быть и то, и другое.
pub async fn get_cabinet_handler<N, C, S>(
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
    MemSync<N, C, S>: SyncOp<N, C, S>,
    S: From<Subscription>,
{
    let grants = match memory.db.peers().grants(subscription_id).await {
        Ok(rows) => rows,
        Err(e) => return db_error("cabinet_grants", e),
    };

    // Same reason as `post_peers_prepare_handler`: a `hysteria2` card is built
    // from a connection token, and a v2 client never creates one itself.
    ensure_granted_creds(&memory, &subscription_id, &grants).await;

    let (cards, connections, devices) = {
        let mem = memory.memory.read().await;
        let creds = card_creds(&mem.connections, &subscription_id);
        let cards = cards_for_grants(&mem.nodes, &grants, &creds);
        let connections = mem
            .connections
            .get_by_subscription_id(&subscription_id)
            .unwrap_or_default()
            .iter()
            .filter(|(_, conn)| !conn.get_deleted())
            .count();
        let devices = mem.peers_state.devices.of_sub(&subscription_id).len();
        (cards, connections, devices)
    };

    // legacy — только старый мир; peers — только гранты; mixed — переезд.
    let scheme = if grants.is_empty() {
        "legacy"
    } else if connections == 0 {
        "peers"
    } else {
        "mixed"
    };

    let access: Vec<serde_json::Value> = grants
        .iter()
        .map(|grant| {
            serde_json::json!({
                "plan": grant.plan,
                "plan_title": plan_title(grant.plan.as_deref()),
                "protos": grant.protos.iter().map(proto_wire_name).collect::<Vec<_>>(),
                "expires_at": grant.expires_at,
                "traffic_limit": grant.traffic_limit,
            })
        })
        .collect();

    let cards_json: Vec<serde_json::Value> = cards
        .iter()
        .map(|card| {
            serde_json::json!({
                "card_id": card.card_id,
                "pool": card.pool,
                "pool_translation": card.pool_translation,
                "country": card.country,
                "country_name": card.country_name,
                "proto": card.proto,
                "nodes": card.nodes.len(),
            })
        })
        .collect();

    // Both sold plans, each with its own term and quota: the page renders
    // «Standard активен / Super Duper — Докупить» from this list instead of
    // guessing the second plan from the subscription row.
    let mut used_by_env: HashMap<String, i64> = HashMap::new();
    for grant in &grants {
        let used = memory
            .db
            .peers()
            .used_bytes(subscription_id, &grant.env)
            .await
            .unwrap_or(0);
        used_by_env.insert(grant.env.to_string(), used);
    }
    let purchasable = purchasable_plans(&memory).await;
    let sub_facts = {
        let mem = memory.memory.read().await;
        mem.subscriptions
            .find_by_id(&subscription_id)
            .map(|sub| crate::plans::SubFacts {
                plan_kind: sub.plan_kind(),
                expires_at: sub.expires_at(),
                limit_bytes: sub.limit_bytes(),
                used_bytes: 0,
            })
    };
    // Only the legacy shape (a subscription without grants) needs the
    // subscription-wide traffic: with grants, every plan has its own number.
    let sub_facts = match sub_facts {
        Some(facts) if grants.is_empty() => {
            let used = memory
                .db
                .traffic()
                .total_for_subscription(subscription_id)
                .await
                .map(|(uplink, downlink)| uplink + downlink)
                .unwrap_or(0);
            Some(crate::plans::SubFacts {
                used_bytes: used,
                ..facts
            })
        }
        other => other,
    };
    let plans = crate::plans::statuses(
        sub_facts.as_ref(),
        &grants,
        &used_by_env,
        &purchasable,
        Utc::now(),
    );

    json_reply(
        200,
        "cabinet",
        serde_json::json!({
            "scheme": scheme,
            "grants": access,
            "plans": plans,
            "cards": cards_json,
            "connections": connections,
            "devices": devices,
        }),
    )
}

/// Which sold plans can take a buyer right now, plan → purchasable.
///
/// A plan is purchasable only when a shard is declared for it *and* that shard
/// has a live machine with address space: selling a plan with nowhere to put
/// the buyer is how a Super Duper purchase ends up on the standard shard.
async fn purchasable_plans<N, C, S>(memory: &MemSync<N, C, S>) -> HashMap<String, bool>
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
    let mut out = HashMap::new();
    for plan in crate::plans::SELLABLE {
        out.insert(
            plan.to_string(),
            !live_shards(memory, plan).await.is_empty(),
        );
    }
    out
}

/// The plan's grant as the cabinet shows it, from the in-memory rights.
fn plan_grant<N, C, S>(
    memory: &MemSync<N, C, S>,
    subscription_id: &Uuid,
    plan: &str,
) -> Option<AccessRow>
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
    let mem = memory.memory.try_read().ok()?;
    mem.peers_state
        .access
        .of_sub(subscription_id)
        .into_iter()
        .find(|grant| grant.plan.as_deref() == Some(plan))
        .cloned()
}

/// 503 with the stable code the cabinet and the app switch on, or `None` when
/// the plan can be sold.
async fn plan_sale_refusal<N, C, S>(
    memory: &MemSync<N, C, S>,
    plan: &str,
) -> Option<(&'static str, usize)>
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
    let declared = memory
        .db
        .peers()
        .plan_shards(plan)
        .await
        .unwrap_or_default()
        .len();
    let live = live_shards(memory, plan).await.len();
    crate::plans::sale_refusal(declared, live).map(|code| (code, declared))
}

/// `POST /subscription/{id}/buy` — what the cabinet needs to start a payment
/// for one plan of an existing subscription.
///
/// It answers the two questions the cabinet must not guess: **may this plan be
/// sold right now** (`503 plan_not_configured` when no shard declares it,
/// `no_live_shard` when the shard has no machine — never a silent fallback onto
/// another plan's shard) and **what does the payment call look like**
/// (`payment.routes` plus the exact body). The price stays where it is today:
/// `[[app.plans]]` of the payment gateway — the api has no prices and must not
/// invent them.
///
/// No auth beyond the path id, exactly like `/cabinet/{id}` and
/// `/subscription/{id}`: the UUID is the capability, and the body cannot name a
/// different subscription.
pub async fn post_buy_plan_handler<N, C, S>(
    subscription_id: Uuid,
    req: BuyPlanRequest,
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
    let Some(plan) = crate::plans::canonical_plan(&req.plan) else {
        return json_reply(
            400,
            "unknown plan",
            serde_json::json!({ "error": "unknown_plan", "plan": req.plan }),
        );
    };

    let exists = {
        let mem = memory.memory.read().await;
        mem.subscriptions.find_by_id(&subscription_id).is_some()
    };
    if !exists {
        return json_reply(
            404,
            "subscription not found",
            serde_json::json!({ "error": "subscription_not_found" }),
        );
    }

    if let Some((code, declared)) = plan_sale_refusal(&memory, plan).await {
        let message = match code {
            "plan_not_configured" => {
                "Этот тариф сейчас не продаётся: для него не объявлен ни один сервер."
            }
            _ => "Сейчас нет доступных серверов для этого тарифа. Попробуйте позже.",
        };
        return json_reply(
            503,
            message,
            serde_json::json!({ "error": code, "plan": plan, "declared_shards": declared }),
        );
    }

    let (active, traffic_limit, expires_at) = match plan_grant(&memory, &subscription_id, plan) {
        Some(grant) => {
            let used = memory
                .db
                .peers()
                .used_bytes(subscription_id, &grant.env)
                .await
                .unwrap_or(0);
            (
                crate::plans::grant_is_active(&grant, used, Utc::now()),
                grant.traffic_limit,
                grant.expires_at,
            )
        }
        None => (false, None, None),
    };

    json_reply(
        200,
        "plan available",
        serde_json::json!({
            "subscription_id": subscription_id,
            "plan": plan,
            "title": crate::plans::title(Some(plan)),
            "active": active,
            "expires_at": expires_at,
            "traffic_limit": traffic_limit,
            "payment": {
                "method": "POST",
                "routes": {
                    "platega": "/payment/platega/subscription/create",
                    "planta": "/payment/planta/subscription/create",
                },
                // Exactly what the cabinet posts to the gateway: the plan is
                // what makes it a plan purchase instead of a plain renewal,
                // and the traffic package is what the lite price is keyed by.
                "body": {
                    "subscription_id": subscription_id,
                    "plan": plan,
                    "traffic_gib": req.traffic_gib,
                },
            },
        }),
    )
}

/// `POST /subscription/{id}/plan` (service token) — payment-gw fulfils a
/// cabinet purchase: create or extend the grant of that plan.
///
/// The purchased period and volume come from the gateway, which owns the price
/// list: `days` is the term (standard) and `traffic_bytes` the volume (Super
/// Duper). The grant is the only place a plan's rights live, so the second plan
/// of a subscription never touches the first one — its own shard, its own term,
/// its own quota.
///
/// `payment_ref` is mandatory: it is the idempotency key. A webhook delivered
/// twice (the provider retries, the gateway retries) answers 200 with
/// `duplicate: true` and changes nothing — the reference is recorded in the
/// grant's `sources` as `pay:<ref>`.
pub async fn post_plan_purchase_handler<N, C, S>(
    subscription_id: Uuid,
    req: PlanPurchaseRequest,
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
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static + From<Subscription>,
    Connection: From<C>,
{
    let Some(plan) = crate::plans::canonical_plan(&req.plan) else {
        return json_reply(
            400,
            "unknown plan",
            serde_json::json!({ "error": "unknown_plan", "plan": req.plan }),
        );
    };

    let exists = {
        let mem = memory.memory.read().await;
        mem.subscriptions.find_by_id(&subscription_id).is_some()
    };
    if !exists {
        return json_reply(
            404,
            "subscription not found",
            serde_json::json!({ "error": "subscription_not_found" }),
        );
    }

    let Some(payment_ref) = req
        .payment_ref
        .as_deref()
        .map(str::trim)
        .filter(|reference| !reference.is_empty() && reference.len() <= 128)
    else {
        return json_reply(
            400,
            "payment_ref is required",
            serde_json::json!({ "error": "payment_ref_required" }),
        );
    };

    // Ten years of access in one purchase is already absurd; anything beyond it
    // is a caller bug that must not reach the date arithmetic.
    if !(0..=MAX_PURCHASE_DAYS).contains(&req.days) {
        return json_reply(
            400,
            "days out of range",
            serde_json::json!({ "error": "bad_days", "max": MAX_PURCHASE_DAYS }),
        );
    }
    let traffic_bytes = req
        .traffic_bytes
        .or_else(|| {
            req.traffic_gib
                .and_then(|gib| gib.checked_mul(GIB))
        })
        .filter(|bytes| *bytes > 0);
    if traffic_bytes.is_some_and(|bytes| bytes < 0) {
        return json_reply(
            400,
            "traffic_bytes must be positive",
            serde_json::json!({ "error": "bad_traffic" }),
        );
    }

    // A purchase that buys neither time nor traffic is a bug on the caller's
    // side, not something to write into a grant.
    if req.days == 0 && traffic_bytes.is_none() {
        return json_reply(
            400,
            "nothing to purchase: pass days and/or traffic_bytes",
            serde_json::json!({ "error": "empty_purchase" }),
        );
    }
    if plan == crate::plans::STANDARD && req.days == 0 {
        // Standard is sold by time; a Super Duper grant never becomes a
        // standard one, so a volume-only standard purchase would be a freebie.
        return json_reply(
            400,
            "the standard plan is sold by time: days is required",
            serde_json::json!({ "error": "days_required" }),
        );
    }

    let protos: Vec<fcore::Tag> = match &req.protos {
        Some(names) => {
            let mut out = Vec::with_capacity(names.len());
            for name in names {
                match peers::tag_from_wire(name) {
                    Some(tag) => out.push(tag),
                    None => {
                        return json_reply(
                            400,
                            "unknown proto",
                            serde_json::json!({ "error": "unknown_proto", "proto": name }),
                        )
                    }
                }
            }
            out
        }
        // Same default the key activation grants: a shard serves both peer
        // protocols, the runtime filters the cards by what its nodes expose.
        None => vec![fcore::Tag::AmneziaPeer, fcore::Tag::WireguardPeer],
    };
    if protos.is_empty() {
        return json_reply(
            400,
            "empty protos",
            serde_json::json!({ "error": "empty_protos" }),
        );
    }

    if let Some((code, declared)) = plan_sale_refusal(&memory, plan).await {
        let message = match code {
            "plan_not_configured" => {
                "Этот тариф сейчас не продаётся: для него не объявлен ни один сервер."
            }
            _ => "Сейчас нет доступных серверов для этого тарифа.",
        };
        return json_reply(
            503,
            message,
            serde_json::json!({ "error": code, "plan": plan, "declared_shards": declared }),
        );
    }

    let shards = live_shards(&memory, plan).await;
    let source = req
        .source
        .as_deref()
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .unwrap_or("payment");
    let marker = crate::plans::payment_marker(payment_ref);

    // The grant names only what the chosen shard serves.
    let announced = announced_inbounds(&memory).await;
    let fulfilled = match memory
        .db
        .peers()
        .fulfil_plan_grant(
            subscription_id,
            plan,
            &protos,
            source,
            &marker,
            req.days,
            traffic_bytes.unwrap_or(0),
            &shards,
            &announced,
        )
        .await
    {
        Ok(fulfilled) => fulfilled,
        Err(e) => return db_error("fulfil_plan_grant", e),
    };

    let Some(row) = fulfilled.row else {
        // Everything was re-checked inside the transaction and nothing can take
        // this plan (a shard went away in between): the payer must not be told
        // they own something nobody serves.
        return json_reply(
            503,
            "Сейчас нет доступных серверов для этого тарифа.",
            serde_json::json!({ "error": "no_live_shard", "plan": plan }),
        );
    };

    if fulfilled.overflow {
        tracing::error!(
            "plan purchase: shards of plan '{}' are over capacity ({}/{}), add a node",
            plan,
            fulfilled.used,
            fulfilled.limit
        );
    }

    // Write-through: `POST /v1/peers` of this very subscription must see the
    // new right without waiting for the next DB sync.
    memory
        .memory
        .write()
        .await
        .peers_state
        .access
        .insert(subscription_id, row.clone());

    // The subscription row itself still carries the term the legacy surfaces
    // read (`/sub`, the app's «expires», the reminder mails). Keep it in step
    // for the time plan only: a lite purchase is a quota and must never make a
    // standard subscription look metered.
    if !fulfilled.duplicate && plan == crate::plans::STANDARD && req.days > 0 {
        if let Err(e) = SyncOp::add_days(&memory, &subscription_id, req.days).await {
            tracing::error!(
                "plan purchase: grant written but subscription {} not extended by {} days: {}",
                subscription_id,
                req.days,
                e
            );
        }
    }

    let used = memory
        .db
        .peers()
        .used_bytes(subscription_id, &row.env)
        .await
        .unwrap_or(0);

    json_reply(
        200,
        if fulfilled.duplicate {
            "plan purchase already fulfilled"
        } else {
            "plan purchase fulfilled"
        },
        serde_json::json!({
            "subscription_id": subscription_id,
            "plan": plan,
            "title": crate::plans::title(Some(plan)),
            "duplicate": fulfilled.duplicate,
            "grant": access_view(&row, used),
            "capacity": {
                "plan": plan,
                "used": fulfilled.used,
                "limit": fulfilled.limit,
                "overflow": fulfilled.overflow,
            },
        }),
    )
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
    let rows = match memory.db.peers().capacity_for_plan(&plan).await {
        Ok(rows) => rows,
        Err(e) => return db_error("capacity_for_plan", e),
    };

    // `max_subscriptions` is an admin override; the real limit is the address
    // budget of the env, counted the same way `live_shards` does.
    let budgets: HashMap<String, i64> = {
        let mem = memory.memory.read().await;
        rows.iter()
            .filter_map(|(env, _, _)| {
                let parsed = env.parse::<Env>().ok()?;
                Some((
                    env.clone(),
                    alloc::allocatable(&mem.peers_state.block_masks(&parsed)) as i64,
                ))
            })
            .collect()
    };

    let shards: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(env, used, override_limit)| {
            let limit = override_limit.unwrap_or_else(|| *budgets.get(&env).unwrap_or(&0));
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

    fn peer_row(pool: Option<&str>) -> PeerRow {
        PeerRow {
            id: uuid::Uuid::new_v4(),
            device_id: uuid::Uuid::new_v4(),
            subscription_id: uuid::Uuid::new_v4(),
            env: Env::from_str("0001").unwrap(),
            proto: fcore::Tag::AmneziaPeer,
            public_key: "KEY".to_string(),
            address: "10.88.0.2".to_string(),
            pool: pool.map(|p| p.to_string()),
            label: None,
            created_at: chrono::Utc::now(),
            revoked_at: None,
            revoke_reason: None,
        }
    }

    /// The pool of the card is the piece a rebuild of the config needs: the
    /// endpoint and the server key live on the node of that pool, not on the
    /// shard at large.
    #[test]
    fn peer_view_names_the_card_it_was_created_for() {
        let row = peer_row(Some("eu"));
        assert_eq!(
            peer_view(&row).card_id.as_deref(),
            Some("0001:eu:amnezia_peer")
        );

        // A tunnel from before `peers.pool` existed has no card to name.
        assert!(peer_view(&peer_row(None)).card_id.is_none());
    }

    fn keyed_row(pool: Option<&str>, public_key: &str, address: &str) -> PeerRow {
        let mut row = peer_row(pool);
        row.public_key = public_key.to_string();
        row.address = address.to_string();
        row
    }

    /// The authoritative set of a node is its own pool, not the env: sending
    /// the whole env is what left a US node's EU address of a device key on the
    /// interface, so the tunnel came up and carried nothing.
    #[test]
    fn a_pool_node_gets_its_pool_and_nothing_else() {
        let rows = vec![
            keyed_row(Some("us"), "KEY", "10.88.0.12"),
            keyed_row(Some("eu"), "OTHER", "10.88.0.3"),
        ];
        let tags = [fcore::Tag::AmneziaPeer];
        let env = Env::from_str("0001").unwrap();

        let us = peers_for_node(&rows, &env, &tags, Some("us"));
        assert_eq!(us.len(), 1);
        assert_eq!(us[0].address, "10.88.0.12");
        assert!(
            us.iter().all(|row| row.pool.as_deref() != Some("eu")),
            "an EU peer must not reach a US node"
        );

        // A node without a pool (pre-pool config) still gets the whole env.
        assert_eq!(peers_for_node(&rows, &env, &tags, None).len(), 2);
        // The tag filter still decides what a node serves at all.
        assert!(peers_for_node(&rows, &env, &[], Some("us")).is_empty());
    }

    /// An own shard is one account: nothing of another subscription may ride
    /// along, whatever a caller hands the selector. The check is keyed on the
    /// stored env, not on the request, so a mixed slice cannot leak.
    #[test]
    fn an_own_shard_sees_only_its_own_subscription() {
        let own: Env = Env::personal_for(
            uuid::Uuid::parse_str("f76c2814-f9ed-4a40-a231-d34ec51f7584").unwrap(),
        );
        let other: Env = Env::personal_for(
            uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
        );

        let mut mine = peer_row(Some("own"));
        mine.env = own.clone();
        mine.public_key = "MINE".to_string();
        mine.address = "10.89.0.2".to_string();

        let mut theirs = peer_row(Some("own"));
        theirs.env = other;
        theirs.public_key = "THEIRS".to_string();
        theirs.address = "10.90.0.2".to_string();

        let rows = vec![mine, theirs];
        let tags = [fcore::Tag::AmneziaPeer];
        let set = peers_for_node(&rows, &own, &tags, Some("own"));

        assert_eq!(set.len(), 1);
        assert_eq!(set[0].public_key, "MINE");
        assert!(
            set.iter().all(|row| row.address.starts_with("10.89.")),
            "an address of another account must never reach this node"
        );
    }

    /// One device key, an address per pool: each side gets exactly its own.
    #[test]
    fn one_key_in_two_pools_keeps_one_address_per_pool() {
        let rows = vec![
            keyed_row(Some("eu"), "KEY", "10.88.0.2"),
            keyed_row(Some("us"), "KEY", "10.88.0.12"),
        ];
        let tags = [fcore::Tag::AmneziaPeer];
        let env = Env::from_str("0001").unwrap();

        let us = peers_for_node(&rows, &env, &tags, Some("us"));
        let eu = peers_for_node(&rows, &env, &tags, Some("eu"));

        assert_eq!(us.len(), 1);
        assert_eq!(eu.len(), 1);
        assert_eq!(us[0].public_key, eu[0].public_key);
        assert_eq!(us[0].address, "10.88.0.12");
        assert_eq!(eu[0].address, "10.88.0.2");
    }

    /// A node with two entry IPs must not be reduced to its primary address:
    /// the card would silently lose an endpoint the client is meant to dial.
    #[test]
    fn a_card_carries_every_entry_ip_of_a_node() {
        let endpoints = card_endpoints(
            vec!["132.243.203.40".to_string(), "132.243.203.41".to_string()],
            443,
        );
        assert_eq!(endpoints, vec!["132.243.203.40:443", "132.243.203.41:443"]);
    }

    #[test]
    fn resolve_card_keeps_the_pool() {
        let Ok((env, proto, pool)) =
            resolve_card(&Some("0001:eu:amnezia_peer".to_string()), None, None)
        else {
            panic!("a well-formed card_id must resolve");
        };
        assert_eq!(env, Env::from_str("0001").unwrap());
        assert_eq!(proto, PeerProto::Amnezia);
        assert_eq!(pool.as_deref(), Some("eu"));

        // The manual form names no pool: `env` + `proto` is not tied to a card.
        let Ok((_, _, pool)) = resolve_card(&None, Some("0001"), Some("wireguard_peer")) else {
            panic!("env + proto must resolve");
        };
        assert!(pool.is_none());

        // A pool-less handle ("<shard>:<pool>:<proto>" with an empty middle) is
        // still a valid card; it just cannot name one.
        let Ok((_, _, pool)) = resolve_card(&Some("0001::amnezia_peer".to_string()), None, None)
        else {
            panic!("an empty pool must still resolve");
        };
        assert!(pool.is_none());
    }

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

    // ------------------------------------------------ v2 cards (kind/payload)

    /// The shape of `POST /v1/peers/prepare`: every prepared pool carries the
    /// card the client builds its config from (the same object `/v1/locations`
    /// hands out), every skipped pool carries a stable reason.
    #[test]
    fn prepare_body_matches_the_contract() {
        let card = PoolCard {
            card_id: "0001:eu:amnezia_peer".to_string(),
            pool: "eu".to_string(),
            pool_translation: None,
            country: Some("NL".to_string()),
            country_name: Some("Netherlands".to_string()),
            proto: "amnezia_peer".to_string(),
            kind: CardKind::Peer,
            nodes: Vec::new(),
        };
        let body = prepare_body(
            &[PreparedPeer {
                card_id: card.card_id.clone(),
                pool: card.pool.clone(),
                proto: card.proto.clone(),
                address: "10.88.0.2/32".to_string(),
                peer_id: Uuid::nil(),
                config: card,
            }],
            &[SkippedPool {
                card_id: "0001:eu:hysteria2".to_string(),
                reason: "card_kind_not_peer".to_string(),
            }],
        );
        println!("{}", body);
        assert_eq!(body["prepared"][0]["card_id"], "0001:eu:amnezia_peer");
        assert_eq!(body["prepared"][0]["address"], "10.88.0.2/32");
        assert_eq!(body["prepared"][0]["config"]["proto"], "amnezia_peer");
        assert!(body["prepared"][0]["config"]["nodes"].is_array());
        assert_eq!(body["skipped"][0]["card_id"], "0001:eu:hysteria2");
        assert_eq!(body["skipped"][0]["reason"], "card_kind_not_peer");
    }

    use fcore::{
        H2Settings, Inbound, Node, NodeStatus, NodeType, Tag, WgKeys, WireguardSettings,
    };
    use std::collections::BTreeMap;

    const H2_HOST: &str = "prem-0001.beacon.frkn.org";
    const SUB_TOKEN: &str = "b1c0c0de-1111-4222-8333-444455556666";

    fn h2_inbound(port: u16, host: &str) -> Inbound {
        Inbound {
            tag: Tag::Hysteria2,
            port,
            stream_settings: None,
            wg: None,
            awg: None,
            h2: Some(H2Settings {
                host: host.to_string(),
                port,
                sni: Some(host.to_string()),
                insecure: false,
                obfs: None,
                alpn: None,
                up_mbps: None,
                down_mbps: None,
                auth_info: None,
            }),
            mtproto_secret: None,
        }
    }

    fn wg_inbound(port: u16) -> Inbound {
        Inbound {
            tag: Tag::WireguardPeer,
            port,
            stream_settings: None,
            wg: Some(WireguardSettings {
                interface: "wg-peers".to_string(),
                address: "10.88.0.1/24".parse().unwrap(),
                port,
                keys: WgKeys::default(),
                dns: vec!["1.1.1.1".parse().unwrap()],
                keepalive: None,
            }),
            awg: None,
            h2: None,
            mtproto_secret: None,
        }
    }

    fn node_with(env: &str, pool: &str, inbound: Inbound) -> Node {
        let tag = inbound.tag;
        Node {
            uuid: Uuid::new_v4(),
            env: Env::from_str(env).unwrap(),
            hostname: H2_HOST.to_string(),
            address: "203.0.113.10".parse().unwrap(),
            status: NodeStatus::Online,
            label: "prem-0001".to_string(),
            interface: "eth0".to_string(),
            created_at: Utc::now(),
            modified_at: Utc::now(),
            inbounds: HashMap::from([(tag, inbound)]),
            cores: 4,
            max_bandwidth_bps: 0,
            country: "DE".to_string(),
            r#type: NodeType::Core,
            cluster: None,
            node_ips: Some(vec!["203.0.113.10".parse().unwrap()]),
            endpoints: Vec::new(),
            endpoints_legacy: false,
            pool: Some(pool.to_string()),
            pool_translation: None,
            pool_country_code: Some("DE".to_string()),
            pool_country_name: Some("Germany".to_string()),
            node_weights: BTreeMap::new(),
            bench_url: None,
        }
    }

    fn storage(nodes: Vec<Node>) -> HashMap<Env, Vec<Node>> {
        let mut out: HashMap<Env, Vec<Node>> = HashMap::new();
        for node in nodes {
            out.entry(node.env.clone()).or_default().push(node);
        }
        out
    }

    fn grant(env: Env, protos: Vec<Tag>) -> AccessRow {
        AccessRow {
            id: Uuid::nil(),
            plan: Some("standard".to_string()),
            env,
            protos,
            sources: vec!["standard".to_string()],
            expires_at: None,
            traffic_limit: None,
        }
    }

    /// Credentials the subscription already holds, keyed as `card_creds` keys
    /// them: `(env, protocol) -> uuid`.
    fn creds_with(env: &Env, tag: Tag, uuid: &str) -> HashMap<(Env, Tag), Uuid> {
        HashMap::from([((env.clone(), tag), Uuid::parse_str(uuid).unwrap())])
    }

    /// A dead premium or self-hosted box keeps its card, a dead core node does
    /// not: the subscriber still sees the machine he owns, while a core entry
    /// that is down would only produce failed connects.
    #[test]
    fn offline_private_nodes_keep_their_card() {
        let env = Env::from_str("0001").unwrap();

        let mut core = node_with("0001", "eu", wg_inbound(51820));
        core.status = NodeStatus::Offline;
        let cards = pool_cards(
            &storage(vec![core]),
            &env,
            CardProto::Peer(PeerProto::Wireguard),
            None,
        );
        assert!(cards.is_empty(), "an offline core node must be hidden");

        let mut prem = node_with("0001", "prem", wg_inbound(51820));
        prem.status = NodeStatus::Offline;
        prem.r#type = NodeType::PremiumNode;
        let cards = pool_cards(
            &storage(vec![prem]),
            &env,
            CardProto::Peer(PeerProto::Wireguard),
            None,
        );
        assert_eq!(cards.len(), 1, "an offline premium node keeps its card");

        let own_env = Env::from_str("selfhosted1").unwrap();
        let mut own = node_with("selfhosted1", "own", wg_inbound(51820));
        own.status = NodeStatus::Offline;
        own.r#type = NodeType::SelfHosted;
        let cards = pool_cards(
            &storage(vec![own]),
            &own_env,
            CardProto::Peer(PeerProto::Wireguard),
            None,
        );
        assert_eq!(
            cards.len(),
            1,
            "an offline self-hosted node keeps its card"
        );
    }

    /// A numbered self-hosted shard holds one node per pool: a node that
    /// declared no pool of its own becomes its own pool (its hostname), so two
    /// machines of one account never collapse into a single `default` card.
    #[test]
    fn numbered_shard_gives_every_node_its_own_card() {
        let env = Env::selfhosted(1);

        let mut berlin = node_with("sf0001", "x", wg_inbound(51820));
        berlin.pool = None;
        berlin.hostname = "home-berlin".to_string();
        berlin.r#type = NodeType::SelfHosted;
        let mut lisbon = node_with("sf0001", "x", wg_inbound(51820));
        lisbon.pool = None;
        lisbon.hostname = "home-lisbon".to_string();
        lisbon.r#type = NodeType::SelfHosted;

        let cards = pool_cards(
            &storage(vec![berlin, lisbon]),
            &env,
            CardProto::Peer(PeerProto::Wireguard),
            None,
        );
        let ids: Vec<&str> = cards.iter().map(|card| card.card_id.as_str()).collect();
        assert_eq!(cards.len(), 2, "one card per node, got {ids:?}");
        assert!(
            ids.iter().any(|id| id.starts_with("sf0001:home-berlin:")),
            "{ids:?}"
        );
        assert!(
            ids.iter().any(|id| id.starts_with("sf0001:home-lisbon:")),
            "{ids:?}"
        );
        assert!(
            ids.iter().all(|id| !id.contains(":default:")),
            "no node may fall back to `default`, got {ids:?}"
        );
    }

    /// A shard whose nodes declare no `pool_translation` still shows the pool's
    /// name: it is borrowed from a sibling shard that does declare one, so `eu`
    /// of `0002` reads `Netherlands` like `eu` of `0001`.
    #[test]
    fn a_pool_name_is_borrowed_from_a_shard_that_declares_it() {
        let mut donor = node_with("0001", "eu", wg_inbound(51820));
        donor.pool_translation = Some(BTreeMap::from([
            ("en".to_string(), "Netherlands".to_string()),
            ("ru".to_string(), "Нидерланды".to_string()),
        ]));
        donor.pool_country_code = Some("NL".to_string());
        donor.pool_country_name = Some("Netherlands".to_string());

        // The second shard has the same pool but no translation of its own.
        let mut bare = node_with("0002", "eu", wg_inbound(51820));
        bare.pool_translation = None;
        bare.pool_country_code = None;
        bare.pool_country_name = None;

        let storage = storage(vec![donor, bare]);
        let mut cards = pool_cards(
            &storage,
            &Env::from_str("0002").unwrap(),
            CardProto::Peer(PeerProto::Wireguard),
            None,
        );
        assert!(cards[0].pool_translation.is_none(), "the node declares none");
        assert!(cards[0].country_name.is_none());

        borrow_pool_names(&mut cards, &storage);
        assert_eq!(
            cards[0].pool_translation.as_ref().unwrap()["en"],
            "Netherlands"
        );
        assert_eq!(cards[0].country_name.as_deref(), Some("Netherlands"));
        // The handle is untouched: the client keeps echoing `0002:eu:...`.
        assert_eq!(cards[0].card_id, "0002:eu:wireguard_peer");
        assert_eq!(cards[0].pool, "eu");
    }

    /// A shard that names its own pool keeps its own name — borrowing never
    /// overwrites a declared value.
    #[test]
    fn a_declared_name_wins_over_a_borrowed_one() {
        let mut donor = node_with("0001", "eu", wg_inbound(51820));
        donor.pool_translation = Some(BTreeMap::from([(
            "en".to_string(),
            "Netherlands".to_string(),
        )]));
        let mut own = node_with("0002", "eu", wg_inbound(51820));
        own.pool_translation = Some(BTreeMap::from([(
            "en".to_string(),
            "Netherlands 2".to_string(),
        )]));

        let storage = storage(vec![donor, own]);
        let mut cards = pool_cards(
            &storage,
            &Env::from_str("0002").unwrap(),
            CardProto::Peer(PeerProto::Wireguard),
            None,
        );
        borrow_pool_names(&mut cards, &storage);
        assert_eq!(
            cards[0].pool_translation.as_ref().unwrap()["en"],
            "Netherlands 2"
        );
    }

    /// Two shards provisioned from the same template sell a pool of the same
    /// name: the row names must differ, or the user sees "Netherlands" twice and
    /// has to open each to find out which shard it is.
    #[test]
    fn pools_with_the_same_name_in_two_shards_are_numbered() {
        let mut a = node_with("0001", "eu", wg_inbound(51820));
        a.pool_translation = Some(BTreeMap::from([
            ("en".to_string(), "Netherlands".to_string()),
            ("ru".to_string(), "Нидерланды".to_string()),
        ]));
        let mut b = node_with("0002", "eu", wg_inbound(51820));
        b.pool_translation = a.pool_translation.clone();

        let cards = pool_cards(
            &storage(vec![a, b]),
            &Env::from_str("0001").unwrap(),
            CardProto::Peer(PeerProto::Wireguard),
            None,
        );
        // One env: `pool_cards` only walks the env it was given, so numbering is
        // a property of the whole card set and is checked on `cards_for_grants`.
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].card_id, "0001:eu:wireguard_peer");

        let mut cards = vec![
            PoolCard {
                card_id: "0001:eu:amnezia_peer".to_string(),
                pool: "eu".to_string(),
                pool_translation: Some(BTreeMap::from([
                    ("en".to_string(), "Netherlands".to_string()),
                    ("ru".to_string(), "Нидерланды".to_string()),
                ])),
                country: Some("NL".to_string()),
                country_name: None,
                proto: "amnezia_peer".to_string(),
                kind: CardKind::Peer,
                nodes: Vec::new(),
            },
            PoolCard {
                card_id: "0002:eu:amnezia_peer".to_string(),
                pool: "eu".to_string(),
                pool_translation: Some(BTreeMap::from([
                    ("en".to_string(), "Netherlands".to_string()),
                    ("ru".to_string(), "Нидерланды".to_string()),
                ])),
                country: Some("NL".to_string()),
                country_name: None,
                proto: "amnezia_peer".to_string(),
                kind: CardKind::Peer,
                nodes: Vec::new(),
            },
        ];
        disambiguate_shared_pools(&mut cards);

        let by_id = |id: &str| {
            cards
                .iter()
                .find(|card| card.card_id == id)
                .unwrap()
                .pool_translation
                .clone()
                .unwrap()
        };
        // The first shard is untouched: its subscribers never see a rename.
        assert_eq!(by_id("0001:eu:amnezia_peer")["en"], "Netherlands");
        assert_eq!(by_id("0001:eu:amnezia_peer")["ru"], "Нидерланды");
        // The second one gets the number.
        assert_eq!(by_id("0002:eu:amnezia_peer")["en"], "Netherlands #2");
        assert_eq!(by_id("0002:eu:amnezia_peer")["ru"], "Нидерланды #2");
        // The handle the client holds must not change: `pool` stays `eu`.
        assert_eq!(cards[0].pool, "eu");
        assert_eq!(cards[1].pool, "eu");
    }

    /// A subscription on a single shard sees exactly what it saw before: no
    /// numbers appear just because numbering exists.
    #[test]
    fn one_shard_is_never_numbered() {
        let mut cards = vec![PoolCard {
            card_id: "0001:eu:amnezia_peer".to_string(),
            pool: "eu".to_string(),
            pool_translation: Some(BTreeMap::from([(
                "en".to_string(),
                "Netherlands".to_string(),
            )])),
            country: Some("NL".to_string()),
            country_name: None,
            proto: "amnezia_peer".to_string(),
            kind: CardKind::Peer,
            nodes: Vec::new(),
        }];
        disambiguate_shared_pools(&mut cards);
        assert_eq!(cards[0].pool_translation.as_ref().unwrap()["en"], "Netherlands");
    }

    /// Two shards whose pools are named differently stay unnumbered, and a pool
    /// without a translation gets one filled in — the row reads `eu #1` instead
    /// of two identical `eu`.
    #[test]
    fn a_pool_without_translation_is_numbered_by_its_bare_name() {
        let mut cards = vec![
            PoolCard {
                card_id: "0001:eu:amnezia_peer".to_string(),
                pool: "eu".to_string(),
                pool_translation: None,
                country: Some("NL".to_string()),
                country_name: None,
                proto: "amnezia_peer".to_string(),
                kind: CardKind::Peer,
                nodes: Vec::new(),
            },
            PoolCard {
                card_id: "0002:eu:amnezia_peer".to_string(),
                pool: "eu".to_string(),
                pool_translation: None,
                country: Some("NL".to_string()),
                country_name: None,
                proto: "amnezia_peer".to_string(),
                kind: CardKind::Peer,
                nodes: Vec::new(),
            },
        ];
        disambiguate_shared_pools(&mut cards);
        // No translation on the first card stays absent — the row still reads
        // `eu`, exactly as it did before the second shard appeared.
        assert!(cards[0].pool_translation.is_none());
        assert_eq!(cards[1].pool_translation.as_ref().unwrap()["en"], "eu #2");
        assert_eq!(cards[0].pool, "eu");
    }

    /// The suffix disappears when the first shard's grant is gone: with a single
    /// card left there is no collision, so the survivor goes back to its plain
    /// name instead of keeping a dangling `#2`.
    #[test]
    fn the_suffix_goes_away_with_the_first_shard() {
        let mut cards = vec![PoolCard {
            card_id: "0002:eu:amnezia_peer".to_string(),
            pool: "eu".to_string(),
            pool_translation: Some(BTreeMap::from([(
                "en".to_string(),
                "Netherlands".to_string(),
            )])),
            country: Some("NL".to_string()),
            country_name: None,
            proto: "amnezia_peer".to_string(),
            kind: CardKind::Peer,
            nodes: Vec::new(),
        }];
        disambiguate_shared_pools(&mut cards);
        assert_eq!(cards[0].pool_translation.as_ref().unwrap()["en"], "Netherlands");
    }

    /// Three shards: the first keeps the name, the rest count from 2.
    #[test]
    fn three_shards_number_from_the_second() {
        let card = |id: &str| PoolCard {
            card_id: id.to_string(),
            pool: "eu".to_string(),
            pool_translation: Some(BTreeMap::from([(
                "en".to_string(),
                "Netherlands".to_string(),
            )])),
            country: Some("NL".to_string()),
            country_name: None,
            proto: "amnezia_peer".to_string(),
            kind: CardKind::Peer,
            nodes: Vec::new(),
        };
        let mut cards = vec![
            card("0003:eu:amnezia_peer"),
            card("0001:eu:amnezia_peer"),
            card("0002:eu:amnezia_peer"),
        ];
        disambiguate_shared_pools(&mut cards);
        // Numbering follows the shard id, not the order the cards arrived in.
        assert_eq!(cards[1].pool_translation.as_ref().unwrap()["en"], "Netherlands");
        assert_eq!(cards[2].pool_translation.as_ref().unwrap()["en"], "Netherlands #2");
        assert_eq!(cards[0].pool_translation.as_ref().unwrap()["en"], "Netherlands #3");
    }

    /// Different names are left alone even across shards.
    #[test]
    fn differently_named_pools_are_not_numbered() {
        let mut cards = vec![
            PoolCard {
                card_id: "0001:eu:amnezia_peer".to_string(),
                pool: "eu".to_string(),
                pool_translation: Some(BTreeMap::from([(
                    "en".to_string(),
                    "Netherlands".to_string(),
                )])),
                country: Some("NL".to_string()),
                country_name: None,
                proto: "amnezia_peer".to_string(),
                kind: CardKind::Peer,
                nodes: Vec::new(),
            },
            PoolCard {
                card_id: "0002:fi:amnezia_peer".to_string(),
                pool: "fi".to_string(),
                pool_translation: Some(BTreeMap::from([(
                    "en".to_string(),
                    "Finland".to_string(),
                )])),
                country: Some("FI".to_string()),
                country_name: None,
                proto: "amnezia_peer".to_string(),
                kind: CardKind::Peer,
                nodes: Vec::new(),
            },
        ];
        disambiguate_shared_pools(&mut cards);
        assert_eq!(cards[0].pool_translation.as_ref().unwrap()["en"], "Netherlands");
        assert_eq!(cards[1].pool_translation.as_ref().unwrap()["en"], "Finland");
    }

    /// A card exists only where the grant and what the nodes announce overlap:
    /// a granted-but-unserved protocol and a served-but-ungranted one both
    /// produce nothing.
    #[test]
    fn cards_are_announced_inbounds_intersected_with_the_grant() {
        let env = Env::from_str("0001").unwrap();
        let nodes = storage(vec![
            node_with("0001", "prem", h2_inbound(8443, H2_HOST)),
            node_with("0001", "prem", wg_inbound(51820)),
        ]);

        // hysteria2 is granted and announced -> a client_config card.
        let cards = cards_for_grants(&nodes, &[grant(env.clone(), vec![Tag::Hysteria2])], &creds_with(&env, Tag::Hysteria2, SUB_TOKEN));
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].card_id, "0001:prem:hysteria2");
        assert_eq!(cards[0].kind, CardKind::ClientConfig);
        assert_eq!(cards[0].nodes.len(), 1);

        // The peer protocol is granted but not announced -> no card.
        let cards = cards_for_grants(&nodes, &[grant(env.clone(), vec![Tag::AmneziaPeer])], &HashMap::new());
        assert!(cards.is_empty(), "an unannounced peer must not produce a card");

        // hysteria2 is announced but not granted -> no card; the granted peer
        // protocol that IS announced keeps its card.
        let cards = cards_for_grants(&nodes, &[grant(env.clone(), vec![Tag::AmneziaPeer, Tag::WireguardPeer])], &creds_with(&env, Tag::Hysteria2, SUB_TOKEN));
        assert!(
            cards.iter().all(|card| card.proto != "hysteria2"),
            "an ungranted protocol must not produce a card"
        );
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].kind, CardKind::Peer);
        assert_eq!(cards[0].card_id, "0001:prem:wireguard_peer");
        assert!(cards[0].nodes[0].payload.is_none());

        // Granted tags without a card implementation (VLESS, M3) are skipped.
        let cards = cards_for_grants(&nodes, &[grant(env, vec![Tag::VlessXhttpCdn])], &HashMap::new());
        assert!(cards.is_empty());
    }

    /// The card the client renders: `kind`, and one payload per node with the
    /// fields of `docs/v2-card-contract.md`.
    #[test]
    fn hysteria2_card_matches_the_contract() {
        let env = Env::from_str("0001").unwrap();
        let nodes = storage(vec![node_with("0001", "prem", h2_inbound(8443, H2_HOST))]);
        let cards = cards_for_grants(&nodes, &[grant(env.clone(), vec![Tag::Hysteria2])], &creds_with(&env, Tag::Hysteria2, SUB_TOKEN));
        assert_eq!(cards.len(), 1);

        let card = serde_json::to_value(&cards[0]).unwrap();
        println!("{}", serde_json::to_string(&card).unwrap());
        assert_eq!(
            card,
            serde_json::json!({
                "card_id": "0001:prem:hysteria2",
                "pool": "prem",
                "pool_translation": null,
                "country": "DE",
                "country_name": "Germany",
                "proto": "hysteria2",
                "kind": "client_config",
                "nodes": [{
                    "endpoint": "203.0.113.10:8443",
                    // Same entry point in the ONE endpoint shape (additive:
                    // `endpoint` stays for old clients).
                    "endpoints": [{ "ip": "203.0.113.10", "port": 8443, "weight": 100 }],
                    "country": "DE",
                    "server_public_key": "",
                    "mtu": 0,
                    "dns": [],
                    "keepalive": null,
                    "obfuscation": null,
                    "payload": {
                        // What the client dials is the node's entry IP; the TLS
                        // name travels in `sni`. A domain in `server` would put
                        // the QUIC handshake back through the tunnel it builds.
                        "server": "203.0.113.10",
                        "port": 8443,
                        "sni": H2_HOST,
                        "auth": SUB_TOKEN,
                        "insecure": false,
                        "obfs": null
                    }
                }]
            })
        );
    }

    /// A node that declares a per-address port (`[[node.endpoints]]`) hands the
    /// client that port — in the card row's `endpoint` string and in the ONE
    /// endpoint record (with its weight and label). The inbound's own port is
    /// the fallback for an address that declares none.
    #[test]
    fn card_node_carries_the_declared_endpoint_port_and_weight() {
        let env = Env::from_str("0001").unwrap();
        let mut node = node_with("0001", "prem", h2_inbound(8443, H2_HOST));
        node.endpoints = vec![
            Endpoint {
                ip: "203.0.113.10".parse().unwrap(),
                port: Some(9443),
                weight: 250,
                label: Some("de-01".to_string()),
            },
            Endpoint {
                ip: "203.0.113.11".parse().unwrap(),
                port: None,
                weight: 100,
                label: None,
            },
        ];
        let nodes = storage(vec![node]);
        let cards = cards_for_grants(
            &nodes,
            &[grant(env.clone(), vec![Tag::Hysteria2])],
            &creds_with(&env, Tag::Hysteria2, SUB_TOKEN),
        );
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].nodes.len(), 2);
        assert_eq!(cards[0].nodes[0].endpoint, "203.0.113.10:9443");
        assert_eq!(cards[0].nodes[0].endpoints[0].port, Some(9443));
        assert_eq!(cards[0].nodes[0].endpoints[0].weight, 250);
        assert_eq!(
            cards[0].nodes[0].endpoints[0].label.as_deref(),
            Some("de-01")
        );
        // No declared port -> the inbound's 8443, not an empty one.
        assert_eq!(cards[0].nodes[1].endpoint, "203.0.113.11:8443");
        assert_eq!(cards[0].nodes[1].endpoints[0].port, Some(8443));
    }

    /// The token is the credential the v1 plane already issues: without one
    /// there is no card, never a made-up password.
    #[test]
    fn hysteria2_card_needs_the_existing_subscription_token() {
        let env = Env::from_str("0001").unwrap();
        let nodes = storage(vec![node_with("0001", "prem", h2_inbound(8443, H2_HOST))]);
        let cards = cards_for_grants(&nodes, &[grant(env, vec![Tag::Hysteria2])], &HashMap::new());
        assert!(cards.is_empty());
    }

    // ------------------------------------------- vless_xhttp_cdn cards

    const XHTTP_HOST: &str = "bs.frkn.org";

    /// `streamSettings` of a `vless_xhttp_cdn` inbound as the node announces
    /// it: xHTTP over plain TLS behind the CDN, padding in `extra.padding`.
    fn xhttp_stream(path: &str, host: Option<&str>) -> serde_json::Value {
        let mut tls = serde_json::json!({ "certificates": [] });
        if let Some(host) = host {
            tls["serverName"] = serde_json::Value::from(host);
            tls["alpn"] = serde_json::json!(["h2"]);
        }
        serde_json::json!({
            "network": "xhttp",
            "xhttpSettings": {
                "path": path,
                "mode": "packet-up",
                "extra": {
                    "padding": { "header": "X-Cache", "key": "_dc", "bytes": "100-1000" }
                }
            },
            "tlsSettings": tls
        })
    }

    /// Built from the announcement JSON itself: the test then also proves the
    /// fields the card reads survive the wire shape the node registers with.
    fn vless_inbound(port: u16, stream: serde_json::Value) -> Inbound {
        serde_json::from_value(serde_json::json!({
            "tag": "VlessXhttpCdn",
            "port": port,
            "streamSettings": stream
        }))
        .expect("a vless_xhttp_cdn announcement must parse")
    }

    /// The card the app renders: `kind`, and one payload per node built from
    /// the announced inbound plus the subscription's own uuid.
    #[test]
    fn vless_xhttp_cdn_card_matches_the_contract() {
        let env = Env::from_str("0001").unwrap();
        let nodes = storage(vec![node_with(
            "0001",
            "prem",
            vless_inbound(443, xhttp_stream("/downloads", Some(XHTTP_HOST))),
        )]);
        let creds = creds_with(&env, Tag::VlessXhttpCdn, SUB_TOKEN);
        let cards = cards_for_grants(
            &nodes,
            &[grant(env.clone(), vec![Tag::VlessXhttpCdn])],
            &creds,
        );
        assert_eq!(cards.len(), 1);

        let card = serde_json::to_value(&cards[0]).unwrap();
        println!("{}", serde_json::to_string(&card).unwrap());
        assert_eq!(
            card,
            serde_json::json!({
                "card_id": "0001:prem:vless_xhttp_cdn",
                "pool": "prem",
                "pool_translation": null,
                "country": "DE",
                "country_name": "Germany",
                "proto": "vless_xhttp_cdn",
                "kind": "client_config",
                "nodes": [{
                    "endpoint": "203.0.113.10:443",
                    "endpoints": [{ "ip": "203.0.113.10", "port": 443, "weight": 100 }],
                    "country": "DE",
                    "server_public_key": "",
                    "mtu": 0,
                    "dns": [],
                    "keepalive": null,
                    "obfuscation": null,
                    "payload": {
                        "uuid": SUB_TOKEN,
                        "transport": "xhttp",
                        "path": "/downloads",
                        "host": XHTTP_HOST,
                        "sni": XHTTP_HOST,
                        "mode": "packet-up",
                        "alpn": ["h2"],
                        "padding": { "header": "X-Cache", "key": "_dc", "bytes": "100-1000" }
                    }
                }]
            })
        );
    }

    /// The uuid is the v1 VLESS credential the subscription already holds:
    /// without such a connection there is no card, and a credential of another
    /// protocol does not substitute for it.
    #[test]
    fn vless_xhttp_cdn_card_needs_the_existing_subscription_uuid() {
        let env = Env::from_str("0001").unwrap();
        let nodes = storage(vec![node_with(
            "0001",
            "prem",
            vless_inbound(443, xhttp_stream("/downloads", Some(XHTTP_HOST))),
        )]);

        let cards = cards_for_grants(
            &nodes,
            &[grant(env.clone(), vec![Tag::VlessXhttpCdn])],
            &HashMap::new(),
        );
        assert!(cards.is_empty(), "no VLESS connection: no uuid, no card");

        let cards = cards_for_grants(
            &nodes,
            &[grant(env, vec![Tag::VlessXhttpCdn])],
            &creds_with(&Env::from_str("0001").unwrap(), Tag::Hysteria2, SUB_TOKEN),
        );
        assert!(
            cards.is_empty(),
            "a hysteria2 token is not a VLESS uuid"
        );
    }

    /// The card can only carry what the node announces: without a path and the
    /// CDN host there is no client config to build.
    #[test]
    fn vless_xhttp_cdn_card_needs_the_announced_path_and_host() {
        let env = Env::from_str("0001").unwrap();
        let creds = creds_with(&env, Tag::VlessXhttpCdn, SUB_TOKEN);

        for stream in [
            xhttp_stream("", Some(XHTTP_HOST)),
            xhttp_stream("/downloads", None),
        ] {
            let nodes = storage(vec![node_with("0001", "prem", vless_inbound(443, stream))]);
            let cards = cards_for_grants(
                &nodes,
                &[grant(env.clone(), vec![Tag::VlessXhttpCdn])],
                &creds,
            );
            assert!(cards.is_empty(), "path and host are required for a card");
        }
    }

    /// A card exists only where the grant and the announcement overlap: the
    /// announced inbound alone produces nothing.
    #[test]
    fn vless_xhttp_cdn_card_needs_the_grant() {
        let env = Env::from_str("0001").unwrap();
        let nodes = storage(vec![node_with(
            "0001",
            "prem",
            vless_inbound(443, xhttp_stream("/downloads", Some(XHTTP_HOST))),
        )]);
        let cards = cards_for_grants(
            &nodes,
            &[grant(env.clone(), vec![Tag::Hysteria2])],
            &creds_with(&env, Tag::VlessXhttpCdn, SUB_TOKEN),
        );
        assert!(
            cards.is_empty(),
            "an ungranted protocol must not produce a card"
        );
    }

    /// `/v1/peers` has nothing to allocate for this card either: 400
    /// `card_kind_not_peer`, never the peer path.
    #[test]
    fn post_peers_refuses_a_vless_xhttp_cdn_card() {
        let err = resolve_card_inner(&Some("0001:prem:vless_xhttp_cdn".to_string()), None, None)
            .expect_err("a client_config card is not a peer card");
        assert_eq!(err, CardError::CardKindNotPeer);
        assert_eq!(err.status(), 400);
        assert_eq!(err.code(), "card_kind_not_peer");

        // The manual form (`env` + `proto`) is refused the same way.
        assert_eq!(
            resolve_card_inner(&None, Some("0001"), Some("vless_xhttp_cdn")),
            Err(CardError::CardKindNotPeer)
        );

        // The proto names itself the same way everywhere the client reads it.
        assert_eq!(CardProto::VlessXhttpCdn.as_str(), "vless_xhttp_cdn");
        assert_eq!(CardProto::VlessXhttpCdn.kind(), CardKind::ClientConfig);
        assert_eq!(
            CardProto::parse("vless_xhttp_cdn"),
            Some(CardProto::VlessXhttpCdn)
        );
        assert_eq!(
            CardProto::from_tag(Tag::VlessXhttpCdn),
            Some(CardProto::VlessXhttpCdn)
        );
    }

    /// `POST /v1/peers/prepare` reports the card as `card_kind_not_peer`
    /// instead of allocating a peer for it (peers::prepare turns `None` into
    /// that refusal code).
    #[test]
    fn prepare_skips_a_vless_xhttp_cdn_card() {
        let env = Env::from_str("0001").unwrap();
        let nodes = storage(vec![node_with(
            "0001",
            "prem",
            vless_inbound(443, xhttp_stream("/downloads", Some(XHTTP_HOST))),
        )]);
        let cards = cards_for_grants(
            &nodes,
            &[grant(env.clone(), vec![Tag::VlessXhttpCdn])],
            &creds_with(&env, Tag::VlessXhttpCdn, SUB_TOKEN),
        );
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].card_id, "0001:prem:vless_xhttp_cdn");
        assert!(prepare_target(&cards[0]).is_none());

        // A peer card of the same env still resolves into a target.
        let peer = PoolCard {
            card_id: "0001:prem:wireguard_peer".to_string(),
            pool: "prem".to_string(),
            pool_translation: None,
            country: None,
            country_name: None,
            proto: "wireguard_peer".to_string(),
            kind: CardKind::Peer,
            nodes: Vec::new(),
        };
        let target = prepare_target(&peer).expect("a peer card is allocatable");
        assert_eq!(target.pool, "prem");
    }

    /// `/v1/peers` has nothing to allocate for a `client_config` card: it must
    /// answer 400 `card_kind_not_peer`, not try the peer path.
    #[test]
    fn post_peers_refuses_a_client_config_card() {
        let err = resolve_card_inner(&Some("0001:prem:hysteria2".to_string()), None, None)
            .expect_err("a client_config card is not a peer card");
        assert_eq!(err, CardError::CardKindNotPeer);
        assert_eq!(err.status(), 400);
        assert_eq!(err.code(), "card_kind_not_peer");

        // The manual form (`env` + `proto`) is refused the same way.
        assert_eq!(
            resolve_card_inner(&None, Some("0001"), Some("hysteria2")),
            Err(CardError::CardKindNotPeer)
        );
        // Garbage stays `unknown_proto`, a peer card still resolves.
        assert_eq!(
            resolve_card_inner(&Some("0001:prem:vless".to_string()), None, None),
            Err(CardError::UnknownProto)
        );
        assert!(resolve_card_inner(&Some("0001:prem:wireguard_peer".to_string()), None, None).is_ok());
    }

    /// An old response has no `kind` and no `payload`: it must read as a peer
    /// card, and its nodes must not grow a `payload` key on the way out.
    #[test]
    fn absent_kind_reads_as_peer() {
        let old = serde_json::json!({
            "card_id": "0001:eu:wireguard_peer",
            "pool": "eu",
            "pool_translation": null,
            "country": "FI",
            "country_name": null,
            "proto": "wireguard_peer",
            "nodes": [{
                "endpoint": "132.243.203.40:51820",
                "country": "FI",
                "server_public_key": "key",
                "mtu": 1420,
                "dns": ["1.1.1.1"],
                "keepalive": null,
                "obfuscation": null
            }]
        });
        let card: PoolCard = serde_json::from_value(old).unwrap();
        assert_eq!(card.kind, CardKind::Peer);
        assert!(card.nodes[0].payload.is_none());

        let json = serde_json::to_value(&card).unwrap();
        assert_eq!(json["kind"], "peer");
        assert!(json["nodes"][0].get("payload").is_none());
        // The wire name of the other kind is the snake_case contract value.
        assert_eq!(
            serde_json::to_value(CardKind::ClientConfig).unwrap(),
            "client_config"
        );
    }
}
