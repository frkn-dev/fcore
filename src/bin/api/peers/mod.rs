//! Peers: the client owns the key pair, the api owns the address.
//!
//! A peer is one tunnel of a device in one env. The device (see `devices`)
//! holds the default key; a peer may carry its own key when a router needs a
//! second independent tunnel in the same env (the kernel keys peers by public
//! key, so two peers with one key on one interface would merge).
//!
//! Everything here is pure logic: no IO, no database, no async. The handlers
//! fetch what is needed and call into these functions; the tests at the bottom
//! pin the rules that matter (allocation, key validation, limits).

pub mod alloc;
pub mod prepare;
pub mod store;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use fcore::{Env, Tag};

/// What a shard is for. Declared in `env_capacity.kind` and **never derived
/// from the name**: a shard called `premium-eu` must not become a premium slot
/// by accident, and a renamed one must not lose its meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShardKind {
    /// A region we operate: may be empty for a while while nodes are added.
    Core,
    /// A premium slot of one account.
    Premium,
    /// An account's own machine (`custompersonal<sub-uuid>`): one shard per
    /// account, one pool (`own`) inside it.
    Selfhosted,
    /// A resell partner's shard (`custom<hex>`): the partner's own machines
    /// plus the users it hosts.
    Resell,
    /// Pre-`selfhosted` spelling of [`Selfhosted`]. Only ever produced from
    /// old `env_capacity` rows; never written back.
    Personal,
}

impl Default for ShardKind {
    fn default() -> Self {
        Self::Core
    }
}

impl ShardKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "core" => Some(Self::Core),
            "premium" => Some(Self::Premium),
            // `personal` is the pre-0031 spelling of the same shard.
            "selfhosted" | "personal" => Some(Self::Selfhosted),
            "resell" => Some(Self::Resell),
            _ => None,
        }
    }

    /// The kind a shard has unless the config says otherwise. `kind` is a
    /// property of the shard, not of the tariff, but for every shard we run it is
    /// fully implied by the plan — declaring it by hand was one more place to
    /// disagree with `plan`.
    pub fn for_plan(plan: &str) -> Option<Self> {
        match plan.trim().to_ascii_lowercase().as_str() {
            "standard" | "lite" => Some(Self::Core),
            "premium" => Some(Self::Premium),
            "selfhosted" => Some(Self::Selfhosted),
            "resell" => Some(Self::Resell),
            _ => None,
        }
    }

    /// A shard of one account's own machines: served only to that account.
    pub fn is_private(&self) -> bool {
        matches!(self, Self::Selfhosted | Self::Personal)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Premium => "premium",
            Self::Selfhosted => "selfhosted",
            Self::Resell => "resell",
            Self::Personal => "personal",
        }
    }
}

/// Migration plane of a subscription (`subscriptions.plane`, migration 0030).
///
/// v1 — the subscription lives on per-protocol `connections` rows.
/// v2 — it holds shard `peers` and no live v1 connection.
/// Dual — both at once: a client that still dials its old config next to a
/// peer. A fresh subscription starts on `V1`; the first live peer moves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Plane {
    V1,
    V2,
    Dual,
}

impl Default for Plane {
    fn default() -> Self {
        Self::V1
    }
}

impl Plane {
    /// Reads the stored value. An absent column, an older backend or an
    /// unexpected string all mean the historical plane: v1.
    pub fn from_db(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("v2") => Self::V2,
            Some("dual") => Self::Dual,
            _ => Self::V1,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
            Self::Dual => "dual",
        }
    }

    /// Plane after this subscription's **first** live peer exists.
    ///
    /// `None` means the row must not change: v2 and dual already record the
    /// migration, and a marker is never moved back. A v1 subscription with a
    /// live v1 connection becomes dual — dropping it to v2 would hide the
    /// client that still uses the old plane.
    pub fn after_peer(self, has_live_connection: bool) -> Option<Self> {
        match self {
            Self::V1 if has_live_connection => Some(Self::Dual),
            Self::V1 => Some(Self::V2),
            Self::V2 | Self::Dual => None,
        }
    }
}

/// Protocol of a peer tunnel. Separate from `Proto` on purpose: `Proto`
/// carries per-connection parameters (a server-side key, an address), while a
/// peer has neither — the key is the client's and the address is assigned by
/// the api.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PeerProto {
    Amnezia,
    Wireguard,
}

impl PeerProto {
    pub fn as_str(self) -> &'static str {
        match self {
            PeerProto::Amnezia => "amnezia_peer",
            PeerProto::Wireguard => "wireguard_peer",
        }
    }

    pub fn tag(self) -> Tag {
        match self {
            PeerProto::Amnezia => Tag::AmneziaPeer,
            PeerProto::Wireguard => Tag::WireguardPeer,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "amnezia_peer" | "amnezia" => Some(PeerProto::Amnezia),
            "wireguard_peer" | "wireguard" => Some(PeerProto::Wireguard),
            _ => None,
        }
    }

    pub fn from_tag(tag: Tag) -> Option<Self> {
        match tag {
            Tag::AmneziaPeer => Some(PeerProto::Amnezia),
            Tag::WireguardPeer => Some(PeerProto::Wireguard),
            _ => None,
        }
    }
}

/// Limits of the peer scheme. Zero means "use the default", mirroring the
/// override rows of `subscription_limits`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Live tunnels of one physical device, counted by its X25519 key across
    /// every subscription and env it holds. A phone keeps one; a router or a
    /// Linux box may keep three up at once for failover.
    pub per_device: i64,
    /// Tunnels of the whole subscription. `0` means **no cap**: with one
    /// address per pool a legal device holds as many rows as the account has
    /// pools, and a fixed number either blocks that (`45` was hit by three
    /// devices on fifteen pools) or is meaningless. The address budget of the
    /// shard is what actually bounds this.
    pub per_subscription: i64,
    /// Installations of the whole subscription.
    pub devices_per_subscription: i64,
}

impl Limits {
    /// One key = one device, so the cap is per device and not per subscription:
    /// a second subscription must not let the same machine open another three
    /// tunnels. Three covers the router case (three tunnels up for failover).
    pub const DEFAULT_PEERS_PER_DEVICE: i64 = 3;
    /// No subscription-wide cap by default: the per-(device, pool) cap is the
    /// real limit, and the shard's address block bounds the rest.
    pub const DEFAULT_PER_SUBSCRIPTION: i64 = 0;
    pub const DEFAULT_DEVICES_PER_SUBSCRIPTION: i64 = 15;

    /// Overrides from `subscription_limits` win over the deployment defaults;
    /// a zero or missing override means "use the default". A zero deployment
    /// default (`peers_per_device = 0` in the config) also falls back to the
    /// constant, so a half-written config cannot refuse every tunnel.
    pub fn resolve(
        defaults: Limits,
        per_device: Option<i64>,
        per_subscription: Option<i64>,
    ) -> Self {
        let default_per_device = if defaults.per_device > 0 {
            defaults.per_device
        } else {
            Self::DEFAULT_PEERS_PER_DEVICE
        };
        Self {
            per_device: per_device.filter(|v| *v > 0).unwrap_or(default_per_device),
            per_subscription: per_subscription
                .filter(|v| *v > 0)
                .unwrap_or(defaults.per_subscription),
            devices_per_subscription: defaults.devices_per_subscription,
        }
    }

    /// Cap of the live tunnels of ONE physical device, counted by its public
    /// key across every subscription and env — not per subscription.
    pub fn check_device_peers(&self, live: i64) -> Result<(), Refusal> {
        if live >= self.per_device {
            return Err(Refusal::PeersLimit(self.per_device));
        }
        Ok(())
    }

    /// The subscription-wide cap is optional: `0` disables it.
    pub fn check_subscription(&self, live: i64) -> Result<(), Refusal> {
        if self.per_subscription > 0 && live >= self.per_subscription {
            return Err(Refusal::SubscriptionLimit(self.per_subscription));
        }
        Ok(())
    }

    /// Checked only when a NEW installation registers: an existing device
    /// creating another tunnel must not be counted twice.
    pub fn check_devices(&self, live: i64) -> Result<(), Refusal> {
        if live >= self.devices_per_subscription {
            return Err(Refusal::DevicesLimit(self.devices_per_subscription));
        }
        Ok(())
    }
}

/// Why a peer request was refused. The HTTP layer maps these to status codes;
/// keeping the decision here means the rules are testable without a socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    BadPublicKey,
    BadLabel,
    NoSubscription,
    /// No `subscription_access` row for this env.
    NoGrant,
    /// The grant exists but does not allow this protocol.
    ProtoNotGranted,
    GrantExpired,
    QuotaExhausted,
    /// Too many live tunnels of ONE physical device (its X25519 key), counted
    /// across every subscription and env.
    PeersLimit(i64),
    SubscriptionLimit(i64),
    /// Too many installations on one subscription.
    DevicesLimit(i64),
    /// Zero live peers free in this env's blocks.
    NoFreeAddress,
    /// Address or key is already live in this env — the caller may retry with
    /// the next free address, which is why this is distinct from a hard error.
    AddressTaken,
    KeyTaken,
    LabelTaken,
    DeviceNotFound,
    PeerNotFound,
    /// The env has no `env_networks` row, so no node can serve it.
    EnvNotServed,
}

impl Refusal {
    pub fn status(&self) -> u16 {
        match self {
            Refusal::BadPublicKey | Refusal::BadLabel => 400,
            Refusal::NoSubscription | Refusal::DeviceNotFound | Refusal::PeerNotFound => 404,
            Refusal::NoGrant | Refusal::ProtoNotGranted | Refusal::EnvNotServed => 403,
            Refusal::GrantExpired | Refusal::QuotaExhausted => 402,
            Refusal::PeersLimit(_)
            | Refusal::SubscriptionLimit(_)
            | Refusal::DevicesLimit(_)
            | Refusal::AddressTaken
            | Refusal::KeyTaken
            | Refusal::LabelTaken => 409,
            Refusal::NoFreeAddress => 503,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Refusal::BadPublicKey => "bad_public_key",
            Refusal::BadLabel => "bad_label",
            Refusal::NoSubscription => "no_subscription",
            Refusal::NoGrant => "no_grant",
            Refusal::ProtoNotGranted => "proto_not_granted",
            Refusal::GrantExpired => "grant_expired",
            Refusal::QuotaExhausted => "quota_exhausted",
            Refusal::PeersLimit(_) => "peers_limit",
            Refusal::SubscriptionLimit(_) => "subscription_limit",
            Refusal::DevicesLimit(_) => "devices_limit",
            Refusal::NoFreeAddress => "no_free_address",
            Refusal::AddressTaken => "address_taken",
            Refusal::KeyTaken => "key_taken",
            Refusal::LabelTaken => "label_taken",
            Refusal::DeviceNotFound => "device_not_found",
            Refusal::PeerNotFound => "peer_not_found",
            Refusal::EnvNotServed => "env_not_served",
        }
    }
}

/// Protocol tag as the panel sends it: `AmneziaWg` (config naming) or the
/// snake_case form the peers contract documents (`amnezia_peer`,
/// `vless_xhttp_cdn`, `hysteria2`). Both must reach the same tag — the site and
/// payment-gw already post CamelCase, the app contract speaks snake_case.
pub fn tag_from_wire(name: &str) -> Option<Tag> {
    if let Ok(tag) = name.parse::<Tag>() {
        return Some(tag);
    }
    let camel: String = name
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    camel.parse::<Tag>().ok()
}

/// A WireGuard/X25519 public key: base64 of exactly 32 bytes.
pub fn is_valid_public_key(value: &str) -> bool {
    use base64::Engine;
    let trimmed = value.trim();
    if trimmed.len() > 64 {
        return false;
    }
    match base64::engine::general_purpose::STANDARD.decode(trimmed) {
        Ok(bytes) => bytes.len() == 32,
        Err(_) => false,
    }
}

/// A user-facing tunnel name (router channels). Keep it short and free of
/// whitespace so it can be shown in `GET /peers` without escaping.
pub fn is_valid_label(value: &str) -> bool {
    let len = value.chars().count();
    (1..=32).contains(&len) && value.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

/// One row of `subscription_access`: what the subscription may use in an env.
#[derive(Clone, Debug)]
pub struct Grant {
    pub env: Env,
    pub protos: Vec<Tag>,
    pub sources: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub traffic_limit: Option<i64>,
}

impl Grant {
    pub fn allows(&self, proto: PeerProto) -> bool {
        self.protos.contains(&proto.tag())
    }

    /// The grant alone decides the term: `subscription.expires_at` is not a
    /// gate for peers, otherwise a later-paid env would be cut short by an
    /// earlier one.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }

    /// `None` = no limit (statistics only).
    pub fn quota_exhausted(&self, used_bytes: i64, now: DateTime<Utc>) -> bool {
        if self.is_expired(now) {
            return true;
        }
        match self.traffic_limit {
            Some(limit) => limit > 0 && used_bytes >= limit,
            None => false,
        }
    }
}

/// Why the api revoked a grant. Stored in `subscription_access.revoked_reason`.
///
/// `expired` and `quota` are the two reasons the api raises on its own (the
/// `expire_grants` task), and both are undone by a renewal: the user paid, so
/// the clock and the meter no longer hold. The other two are explicit
/// decisions — a support revocation and a subscription that was ended on
/// purpose — and a longer term must not quietly undo them.
pub const REVOKE_EXPIRED: &str = "expired";
pub const REVOKE_QUOTA: &str = "quota";
pub const REVOKE_USER: &str = "user";
pub const REVOKE_SUBSCRIPTION_ENDED: &str = "subscription_ended";
/// `reap_idle_peers`: the tunnel carried no booked traffic for the configured
/// number of days. Not a decision of the customer — the peer is simply gone
/// from the device — so a re-created tunnel is a normal create, not a revival.
pub const REVOKE_IDLE: &str = "idle";
/// `reap_idle_peers`: a live peer survived a grant revocation (a node was
/// offline and missed the bulk delete). The reason recorded is the sweep's,
/// not the original decision — the grant row keeps that.
pub const REVOKE_GRANT_REVOKED: &str = "grant_revoked";

/// Is there room under the grant's traffic limit? `None` (or a non-positive
/// limit) means the env is not metered at all.
pub fn quota_has_room(limit: Option<i64>, used_bytes: i64) -> bool {
    match limit {
        Some(limit) => limit <= 0 || used_bytes < limit,
        None => true,
    }
}

/// Why a grant row is not live. Split from a bare string so a renewal can
/// treat the legacy NULL reason (rows revoked before the column existed)
/// without ever reviving an explicit revocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Revocation {
    /// `expire_grants` on a term that ran out.
    Expired,
    /// `expire_grants` on a spent quota.
    Quota,
    /// `user`, `subscription_ended` or anything we do not recognise.
    Explicit(String),
    /// Revoked without a reason (before the column existed): the clock says
    /// whether it was an expiry, otherwise it was done on purpose.
    Legacy,
}

impl Revocation {
    pub fn from_reason(reason: Option<&str>) -> Option<Self> {
        let reason = reason?;
        Some(match reason {
            REVOKE_EXPIRED => Self::Expired,
            REVOKE_QUOTA => Self::Quota,
            other => Self::Explicit(other.to_string()),
        })
    }
}

/// A grant as a renewal sees it: the row plus whether it is live or why it
/// was revoked. This is the decision a renewal makes, pure and testable; the
/// storage layer locks the rows and writes the result.
#[derive(Clone, Debug)]
pub struct GrantState {
    pub grant: Grant,
    pub revocation: Option<Revocation>,
}

impl GrantState {
    pub fn live(grant: Grant) -> Self {
        Self {
            grant,
            revocation: None,
        }
    }

    /// `reason = None` with a revocation set means a pre-`revoked_reason` row.
    pub fn revoked(grant: Grant, reason: Option<&str>) -> Self {
        Self {
            grant,
            revocation: Some(
                Revocation::from_reason(reason).unwrap_or(Revocation::Legacy),
            ),
        }
    }

    /// What a renewal leaves live, or `None` when the row must stay as it is.
    ///
    /// A live row follows the subscription only forward (`GREATEST` of its own
    /// term and the new one, `NULL` stays forever). A revoked row comes back
    /// only when the clock or the meter killed it, and a spent quota has to
    /// have room again — reviving it would just flare on the next expiry tick.
    pub fn renewed(
        &self,
        expires_at: DateTime<Utc>,
        used_bytes: i64,
    ) -> Option<Grant> {
        let mut renewed = self.grant.clone();
        match &self.revocation {
            None => {
                let old = self.grant.expires_at?;
                if old >= expires_at {
                    return None;
                }
                renewed.expires_at = Some(expires_at);
            }
            Some(Revocation::Expired) | Some(Revocation::Legacy) => {
                if let Some(Revocation::Legacy) = self.revocation {
                    // No reason to go on: only a row whose term is over was an
                    // expiry; a live term means it was ended on purpose.
                    if !self.grant.is_expired(Utc::now()) {
                        return None;
                    }
                }
                renewed.expires_at = match self.grant.expires_at {
                    Some(_) => Some(expires_at),
                    // A quota grant has no term at all — keep it that way.
                    None => None,
                };
            }
            Some(Revocation::Quota) => {
                if !quota_has_room(self.grant.traffic_limit, used_bytes) {
                    return None;
                }
                renewed.expires_at = match self.grant.expires_at {
                    Some(_) => Some(expires_at),
                    None => None,
                };
            }
            Some(Revocation::Explicit(_)) => return None,
        }
        Some(renewed)
    }
}

/// Validate a create request against a grant without touching the database.
pub fn check_grant(
    grant: Option<&Grant>,
    proto: PeerProto,
    now: DateTime<Utc>,
) -> Result<(), Refusal> {
    let Some(grant) = grant else {
        return Err(Refusal::NoGrant);
    };
    if !grant.allows(proto) {
        return Err(Refusal::ProtoNotGranted);
    }
    if grant.is_expired(now) {
        return Err(Refusal::GrantExpired);
    }
    Ok(())
}

// ---------------------------------------------------------------- peer cleanup

/// Why the cleanup job drops a live tunnel. `Idle` is the clock on the
/// traffic watermark; the other two are rights, not age.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerReap {
    /// No booked traffic for `idle_days`: the address belongs to the subject
    /// shard, not to a device that stopped connecting (or never did).
    Idle,
    /// The owning subscription is over — deleted, or its term is over with
    /// nothing left on the traffic balance.
    SubscriptionEnded,
    /// The grant that authorized this env was revoked.
    GrantRevoked,
}

/// What the cleanup knows about one live peer.
#[derive(Clone, Debug)]
pub struct PeerUsage {
    pub created_at: DateTime<Utc>,
    /// Last time traffic was booked for this peer (`peer_traffic` /
    /// `peer_watermarks`); `None` when it never moved a byte.
    pub last_traffic_at: Option<DateTime<Utc>>,
}

impl PeerUsage {
    /// The clock the idle rule counts from: real traffic when there is any,
    /// otherwise the tunnel's creation, so a peer that never connected still
    /// ages out instead of living forever.
    pub fn idle_since(&self) -> DateTime<Utc> {
        self.last_traffic_at.unwrap_or(self.created_at)
    }
}

/// What the cleanup knows about the peer's subscription and its grant.
#[derive(Clone, Copy, Debug, Default)]
pub struct PeerOwner {
    pub deleted: bool,
    /// `expires_at` is set and already over.
    pub expired: bool,
    /// Traffic mode with bytes left on the balance: the paid term is over but
    /// the subscription still has a right to its tunnels.
    pub kept_by_traffic: bool,
    /// The env is the subscription's own (self-hosted node, premium slot): no
    /// grant row exists for it and none is needed.
    pub env_owned: bool,
    pub grant_live: bool,
    /// Any grant row (live or revoked) exists for this (subscription, env):
    /// rows are only ever written for shared shards, never for an owned env.
    pub grant_seen: bool,
}

/// The cleanup decision. Pure, so the rules are testable without Postgres; the
/// job only gathers [`PeerUsage`] / [`PeerOwner`] and revokes what this
/// returns. Precedence: an ended subscription outranks a revoked grant, and
/// both outrank idleness — the logged reason should be the real one.
pub fn reap_reason(
    usage: &PeerUsage,
    owner: &PeerOwner,
    now: DateTime<Utc>,
    idle_days: i64,
) -> Option<PeerReap> {
    // A live grant is `expire_grants`' business: it closes the grant on its
    // own term/quota tick and takes the env's peers with it, so this sweep
    // must not race it. What is left here is the peer nobody else owns — an
    // env without a grant (own node, selfhosted user) behind a subscription
    // that is deleted or expired with no balance to live on.
    if owner.deleted
        || (owner.expired && !owner.grant_live && !owner.kept_by_traffic)
    {
        return Some(PeerReap::SubscriptionEnded);
    }
    if owner.grant_seen && !owner.grant_live && !owner.env_owned {
        return Some(PeerReap::GrantRevoked);
    }
    if idle_days > 0 && usage.idle_since() <= now - chrono::Duration::days(idle_days) {
        return Some(PeerReap::Idle);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_kind_parses_and_round_trips() {
        for kind in [
            ShardKind::Core,
            ShardKind::Premium,
            ShardKind::Selfhosted,
            ShardKind::Resell,
        ] {
            assert_eq!(ShardKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ShardKind::parse(" CORE "), Some(ShardKind::Core));
        assert_eq!(ShardKind::parse("other"), None);
        // The pre-0031 spelling is the same shard.
        assert_eq!(
            ShardKind::parse("personal"),
            Some(ShardKind::Selfhosted)
        );
        // A shard without a declared kind is a region we operate.
        assert_eq!(ShardKind::default(), ShardKind::Core);
        // Account-owned shards: one own-node account, one reseller.
        assert!(ShardKind::Selfhosted.is_private());
        assert!(ShardKind::Personal.is_private());
        assert!(!ShardKind::Resell.is_private());
        assert!(!ShardKind::Core.is_private());
        assert_eq!(ShardKind::for_plan("selfhosted"), Some(ShardKind::Selfhosted));
        assert_eq!(ShardKind::for_plan("resell"), Some(ShardKind::Resell));
    }
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()
    }

    fn days_ago(days: i64) -> DateTime<Utc> {
        now() - chrono::Duration::days(days)
    }

    fn active_owner() -> PeerOwner {
        PeerOwner {
            grant_live: true,
            ..PeerOwner::default()
        }
    }

    /// A peer that moved traffic 31 days ago holds a shard address for
    /// nothing: the cleanup drops it on age alone.
    #[test]
    fn peer_idle_for_31_days_is_reaped() {
        let usage = PeerUsage {
            created_at: days_ago(90),
            last_traffic_at: Some(days_ago(31)),
        };
        assert_eq!(
            reap_reason(&usage, &active_owner(), now(), 30),
            Some(PeerReap::Idle)
        );
    }

    /// A live tunnel used 5 days ago stays: the idle rule is 30 days, and
    /// one pass must never cut a working device.
    #[test]
    fn peer_used_5_days_ago_is_kept() {
        let usage = PeerUsage {
            created_at: days_ago(90),
            last_traffic_at: Some(days_ago(5)),
        };
        assert_eq!(reap_reason(&usage, &active_owner(), now(), 30), None);
    }

    /// An expired subscription loses its grant-less tunnels immediately,
    /// whatever the traffic says — a peer of an account that stopped paying
    /// is not ours to keep. Two things keep a peer: a traffic balance it may
    /// still spend, and a live grant, which `expire_grants` closes on its own
    /// tick (this sweep must not race it).
    #[test]
    fn peer_of_expired_subscription_is_reaped_regardless_of_age() {
        let fresh = PeerUsage {
            created_at: days_ago(1),
            last_traffic_at: Some(days_ago(1)),
        };
        let expired = PeerOwner {
            expired: true,
            ..PeerOwner::default()
        };
        assert_eq!(
            reap_reason(&fresh, &expired, now(), 30),
            Some(PeerReap::SubscriptionEnded)
        );

        // Traffic mode with balance left: the term is over but the right is
        // not, so age (not the clock) decides.
        let kept = PeerOwner {
            kept_by_traffic: true,
            ..expired
        };
        assert_eq!(reap_reason(&fresh, &kept, now(), 30), None);

        // A granted env belongs to `expire_grants`: it revokes the grant and
        // the env's peers together, so the sweep leaves it alone.
        let granted = PeerOwner {
            grant_live: true,
            ..expired
        };
        assert_eq!(reap_reason(&fresh, &granted, now(), 30), None);
    }

    /// A peer that never moved a byte counts from `created_at`: no traffic
    /// rows at all must not mean "no evidence, keep forever".
    #[test]
    fn peer_without_traffic_ages_from_creation() {
        let usage = PeerUsage {
            created_at: days_ago(31),
            last_traffic_at: None,
        };
        assert_eq!(
            reap_reason(&usage, &active_owner(), now(), 30),
            Some(PeerReap::Idle)
        );

        let young = PeerUsage {
            created_at: days_ago(3),
            last_traffic_at: None,
        };
        assert_eq!(reap_reason(&young, &active_owner(), now(), 30), None);
    }

    /// A revoked grant takes the tunnels of that env with it, but an env the
    /// subscription owns (its own node) has no grant row by design.
    #[test]
    fn revoked_grant_reaps_but_an_owned_env_does_not() {
        let fresh = PeerUsage {
            created_at: days_ago(1),
            last_traffic_at: Some(days_ago(1)),
        };
        let revoked = PeerOwner {
            grant_seen: true,
            ..PeerOwner::default()
        };
        assert_eq!(
            reap_reason(&fresh, &revoked, now(), 30),
            Some(PeerReap::GrantRevoked)
        );

        let owned = PeerOwner {
            env_owned: true,
            ..revoked
        };
        assert_eq!(reap_reason(&fresh, &owned, now(), 30), None);
    }

    /// `peer_idle_days = 0` is the kill switch: rights are still enforced,
    /// age is not.
    #[test]
    fn zero_idle_days_disables_only_the_age_rule() {
        let old = PeerUsage {
            created_at: days_ago(400),
            last_traffic_at: Some(days_ago(400)),
        };
        assert_eq!(reap_reason(&old, &active_owner(), now(), 0), None);
        let expired = PeerOwner {
            expired: true,
            ..PeerOwner::default()
        };
        assert_eq!(
            reap_reason(&old, &expired, now(), 0),
            Some(PeerReap::SubscriptionEnded)
        );
    }

    fn grant(protos: Vec<Tag>, expires_at: Option<DateTime<Utc>>, limit: Option<i64>) -> Grant {
        Grant {
            env: Env::Ru,
            protos,
            sources: vec!["standard".to_string()],
            expires_at,
            traffic_limit: limit,
        }
    }

    #[test]
    fn accepts_a_real_32_byte_key() {
        // 32 zero bytes, base64.
        assert!(is_valid_public_key(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        ));
    }

    #[test]
    fn refuses_broken_keys() {
        assert!(!is_valid_public_key(""));
        assert!(!is_valid_public_key("not-base64!!"));
        assert!(!is_valid_public_key("AAAA"));
        // 31 bytes
        assert!(!is_valid_public_key(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="
        ));
    }

    #[test]
    fn labels_are_short_and_clean() {
        assert!(is_valid_label("ch2"));
        assert!(is_valid_label("Роутер-дача"));
        assert!(!is_valid_label(""));
        assert!(!is_valid_label("two words"));
        assert!(!is_valid_label(&"x".repeat(33)));
    }

    #[test]
    fn grant_term_is_its_own() {
        let live = grant(
            vec![Tag::AmneziaPeer],
            Some(now() + chrono::Duration::days(1)),
            None,
        );
        assert!(!live.is_expired(now()));
        let expired = grant(
            vec![Tag::AmneziaPeer],
            Some(now() - chrono::Duration::days(1)),
            None,
        );
        assert!(expired.is_expired(now()));
        // NULL = forever
        let forever = grant(vec![Tag::AmneziaPeer], None, None);
        assert!(!forever.is_expired(now()));
    }

    #[test]
    fn quota_counts_up_to_the_limit() {
        let g = grant(vec![Tag::AmneziaPeer], None, Some(1024));
        assert!(!g.quota_exhausted(1023, now()));
        assert!(g.quota_exhausted(1024, now()));
        // no limit = never exhausted by traffic
        let unlimited = grant(vec![Tag::AmneziaPeer], None, None);
        assert!(!unlimited.quota_exhausted(i64::MAX, now()));
    }

    #[test]
    fn proto_must_be_granted() {
        let g = grant(vec![Tag::AmneziaPeer], None, None);
        assert_eq!(check_grant(Some(&g), PeerProto::Amnezia, now()), Ok(()));
        assert_eq!(
            check_grant(Some(&g), PeerProto::Wireguard, now()),
            Err(Refusal::ProtoNotGranted)
        );
        assert_eq!(
            check_grant(None, PeerProto::Amnezia, now()),
            Err(Refusal::NoGrant)
        );
    }

    #[test]
    fn renewal_revives_an_expired_grant() {
        let old_term = now() - chrono::Duration::days(3);
        let state = GrantState::revoked(
            grant(vec![Tag::AmneziaPeer], Some(old_term), None),
            Some(REVOKE_EXPIRED),
        );
        let new_term = now() + chrono::Duration::days(30);
        let revived = state
            .renewed(new_term, 0)
            .expect("a grant killed by the clock must come back on renewal");
        assert_eq!(revived.expires_at, Some(new_term));
        assert_eq!(revived.env, Env::Ru);
        assert_eq!(revived.protos, vec![Tag::AmneziaPeer]);
        assert_eq!(revived.traffic_limit, None);
    }

    #[test]
    fn renewal_leaves_an_explicit_revocation_alone() {
        let past = now() - chrono::Duration::days(3);
        for reason in [
            REVOKE_USER,
            REVOKE_SUBSCRIPTION_ENDED,
            "something_support_invented",
        ] {
            let state = GrantState::revoked(
                grant(vec![Tag::AmneziaPeer], Some(past), None),
                Some(reason),
            );
            assert!(
                state
                    .renewed(now() + chrono::Duration::days(30), 0)
                    .is_none(),
                "{reason} was an explicit decision and must stay revoked"
            );
        }
    }

    #[test]
    fn renewal_without_grants_is_a_no_op() {
        // A v1 subscription (or a grant that was never created) has no rows:
        // the renewal has nothing to extend, revive or create.
        let rows: Vec<GrantState> = Vec::new();
        let renewed: Vec<Grant> = rows
            .iter()
            .filter_map(|state| state.renewed(now() + chrono::Duration::days(30), 0))
            .collect();
        assert!(renewed.is_empty());

        let store = crate::peers::store::AccessStore::default();
        assert!(store.is_empty_for(&uuid::Uuid::nil()));
    }

    #[test]
    fn a_revived_grant_passes_the_peers_access_check() {
        let state = GrantState::revoked(
            grant(
                vec![Tag::AmneziaPeer],
                Some(now() - chrono::Duration::days(1)),
                None,
            ),
            Some(REVOKE_EXPIRED),
        );
        let revived = state
            .renewed(now() + chrono::Duration::days(30), 0)
            .expect("expired → live");
        // Same env and protos as before the revocation: the client lands on
        // the same shard, which is the whole point of reviving the row.
        assert_eq!(check_grant(Some(&revived), PeerProto::Amnezia, now()), Ok(()));
        assert_eq!(
            check_grant(Some(&revived), PeerProto::Wireguard, now()),
            Err(Refusal::ProtoNotGranted)
        );
        // And it is no longer refused as expired, unlike the old row.
        assert_eq!(
            check_grant(Some(&state.grant), PeerProto::Amnezia, now()),
            Err(Refusal::GrantExpired)
        );
    }

    #[test]
    fn a_spent_quota_stays_revoked_until_there_is_room() {
        let spent = GrantState::revoked(
            grant(vec![Tag::AmneziaPeer], None, Some(1000)),
            Some(REVOKE_QUOTA),
        );
        assert!(spent.renewed(now(), 1000).is_none());
        assert!(spent.renewed(now(), 4000).is_none());
        let revived = spent.renewed(now(), 999).expect("room under the limit");
        // A quota grant has no term: a renewal must not invent one.
        assert_eq!(revived.expires_at, None);
        assert_eq!(revived.traffic_limit, Some(1000));
        assert_eq!(check_grant(Some(&revived), PeerProto::Amnezia, now()), Ok(()));
    }

    #[test]
    fn a_live_grant_only_moves_forward() {
        let term = now() + chrono::Duration::days(10);
        let live = GrantState::live(grant(vec![Tag::AmneziaPeer], Some(term), None));
        assert!(live.renewed(term, 0).is_none(), "same term: nothing to do");
        assert!(live
            .renewed(term - chrono::Duration::days(5), 0)
            .is_none());
        let later = now() + chrono::Duration::days(40);
        assert_eq!(live.renewed(later, 0).unwrap().expires_at, Some(later));

        // NULL term = forever, a renewal leaves it alone.
        let forever = GrantState::live(grant(vec![Tag::AmneziaPeer], None, Some(10)));
        assert!(forever.renewed(later, 0).is_none());
    }

    #[test]
    fn a_pre_column_revocation_is_judged_by_its_term() {
        // Rows revoked before `revoked_reason` existed: a term already over was
        // the expiry task, a term still running means someone ended it.
        let expired = GrantState::revoked(
            grant(
                vec![Tag::AmneziaPeer],
                Some(Utc::now() - chrono::Duration::days(1)),
                None,
            ),
            None,
        );
        let new_term = Utc::now() + chrono::Duration::days(30);
        assert_eq!(
            expired.renewed(new_term, 0).unwrap().expires_at,
            Some(new_term)
        );

        let ended = GrantState::revoked(
            grant(
                vec![Tag::AmneziaPeer],
                Some(Utc::now() + chrono::Duration::days(5)),
                None,
            ),
            None,
        );
        assert!(ended.renewed(new_term, 0).is_none());
    }

    #[test]
    fn limits_resolve_defaults_and_overrides() {
        let defaults = Limits {
            per_device: 3,
            per_subscription: 45,
            devices_per_subscription: 15,
        };
        assert_eq!(Limits::resolve(defaults, None, None), defaults);
        assert_eq!(Limits::resolve(defaults, Some(0), Some(0)), defaults);
        assert_eq!(
            Limits::resolve(defaults, Some(10), Some(200)),
            Limits {
                per_device: 10,
                per_subscription: 200,
                devices_per_subscription: 15
            }
        );
        // Deployment-wide defaults are honoured when there is no override.
        let wide = Limits {
            per_device: 10,
            per_subscription: 500,
            devices_per_subscription: 15,
        };
        assert_eq!(Limits::resolve(wide, None, None), wide);
        // `peers_per_device = 0` in the config means "default", never "no
        // tunnels at all": the constant takes over.
        let zeroed = Limits {
            per_device: 0,
            per_subscription: 45,
            devices_per_subscription: 15,
        };
        assert_eq!(
            Limits::resolve(zeroed, None, None).per_device,
            Limits::DEFAULT_PEERS_PER_DEVICE
        );
    }

    #[test]
    fn limit_checks_count_live_only() {
        let limits = Limits::resolve(
            Limits {
                per_device: 3,
                per_subscription: 45,
                devices_per_subscription: 15,
            },
            None,
            None,
        );
        assert!(limits.check_device_peers(2).is_ok());
        assert_eq!(limits.check_device_peers(3), Err(Refusal::PeersLimit(3)));
        assert!(limits.check_subscription(44).is_ok());
        assert_eq!(
            limits.check_subscription(45),
            Err(Refusal::SubscriptionLimit(45))
        );
    }

    #[test]
    fn tags_are_accepted_in_both_namings() {
        assert_eq!(tag_from_wire("amnezia_peer"), Some(Tag::AmneziaPeer));
        assert_eq!(tag_from_wire("wireguard_peer"), Some(Tag::WireguardPeer));
        assert_eq!(tag_from_wire("vless_xhttp_cdn"), Some(Tag::VlessXhttpCdn));
        assert_eq!(tag_from_wire("hysteria2"), Some(Tag::Hysteria2));
        // The CamelCase form the site and payment-gw already post.
        assert_eq!(tag_from_wire("AmneziaWg"), Some(Tag::AmneziaWg));
        assert_eq!(tag_from_wire("Wireguard"), Some(Tag::Wireguard));
        assert_eq!(tag_from_wire("nonsense"), None);
    }

    #[test]
    fn limits_cap_peers_and_devices() {
        let limits = Limits {
            per_device: 3,
            per_subscription: 45,
            devices_per_subscription: 15,
        };
        assert!(limits.check_device_peers(2).is_ok());
        assert_eq!(limits.check_device_peers(3), Err(Refusal::PeersLimit(3)));
        assert!(limits.check_subscription(44).is_ok());
        assert_eq!(
            limits.check_subscription(45),
            Err(Refusal::SubscriptionLimit(45))
        );
        assert!(limits.check_devices(14).is_ok());
        assert_eq!(limits.check_devices(15), Err(Refusal::DevicesLimit(15)));
    }

    #[test]
    fn refusal_codes_are_stable() {
        assert_eq!(Refusal::PeersLimit(3).status(), 409);
        assert_eq!(Refusal::PeersLimit(3).code(), "peers_limit");
        // The device cap and the subscriber device cap stay distinct codes: a
        // client shows "this device has too many tunnels" for one and "delete a
        // device" for the other.
        assert_eq!(Refusal::DevicesLimit(15).code(), "devices_limit");
        assert_eq!(Refusal::GrantExpired.status(), 402);
        assert_eq!(Refusal::NoGrant.status(), 403);
        assert_eq!(Refusal::NoFreeAddress.status(), 503);
        assert_eq!(Refusal::BadPublicKey.status(), 400);
    }
}

#[cfg(test)]
mod subscription_cap_tests {
    use super::*;

    fn limits(per_subscription: i64) -> Limits {
        Limits {
            per_device: 3,
            per_subscription,
            devices_per_subscription: 15,
        }
    }

    /// One address per pool is the normal shape: 15 pools × 3 devices must not
    /// trip a subscription-wide counter, so `0` has to mean "no cap".
    #[test]
    fn zero_means_no_subscription_cap() {
        let l = limits(0);
        assert!(l.check_subscription(1_000).is_ok());
        assert!(l.check_subscription(i64::MAX).is_ok());
    }

    /// An explicit number still throttles, and the per-(device, pool) cap is
    /// unaffected by either setting.
    #[test]
    fn an_explicit_cap_still_bites() {
        let l = limits(5);
        assert!(l.check_subscription(4).is_ok());
        assert!(matches!(
            l.check_subscription(5),
            Err(Refusal::SubscriptionLimit(5))
        ));
        assert!(l.check_device_peers(2).is_ok());
        assert!(matches!(
            l.check_device_peers(3),
            Err(Refusal::PeersLimit(3))
        ));
    }
}

#[cfg(test)]
mod plane_tests {
    use super::Plane;

    /// (a) A fresh subscription is on the v1 plane: the column defaults to
    /// `v1`, and an older backend/absent column must read the same way.
    #[test]
    fn a_fresh_subscription_is_on_the_v1_plane() {
        assert_eq!(Plane::from_db(None), Plane::V1);
        assert_eq!(Plane::from_db(Some("v1")), Plane::V1);
        assert_eq!(Plane::default(), Plane::V1);
        assert_eq!(Plane::V1.as_str(), "v1");
    }

    /// (b) The first live peer moves a pure v1 subscription to v2.
    #[test]
    fn the_first_peer_moves_a_subscription_to_v2() {
        assert_eq!(Plane::V1.after_peer(false), Some(Plane::V2));
        assert_eq!(Plane::from_db(Some("v1")).after_peer(false), Some(Plane::V2));
    }

    /// (c) A subscription that also still has a live v1 connection is dual:
    /// calling it v2 would hide the client that still uses the old plane.
    #[test]
    fn a_peer_next_to_live_connections_makes_the_plane_dual() {
        assert_eq!(Plane::V1.after_peer(true), Some(Plane::Dual));
    }

    /// A recorded migration is never undone by another peer, and every stored
    /// value round-trips.
    #[test]
    fn a_marked_subscription_is_never_moved_back() {
        for plane in [Plane::V2, Plane::Dual] {
            assert_eq!(plane.after_peer(false), None);
            assert_eq!(plane.after_peer(true), None);
            assert_eq!(Plane::from_db(Some(plane.as_str())), plane);
        }
        // Unknown values degrade to the historical plane instead of failing.
        assert_eq!(Plane::from_db(Some("future")), Plane::V1);
    }
}
