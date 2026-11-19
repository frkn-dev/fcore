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
    /// A user's own machine (self-hosted node).
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
            "personal" => Some(Self::Personal),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Premium => "premium",
            Self::Personal => "personal",
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
    /// Tunnels of one installation (a phone keeps one per shard it uses).
    pub per_device: i64,
    /// Tunnels of the whole subscription — the hard cap on the address budget.
    pub per_subscription: i64,
    /// Installations of the whole subscription.
    pub devices_per_subscription: i64,
}

impl Limits {
    pub const DEFAULT_PER_DEVICE: i64 = 3;
    /// 15 devices × 3 tunnels: the peer cap must not bind before the device cap.
    pub const DEFAULT_PER_SUBSCRIPTION: i64 = 45;
    pub const DEFAULT_DEVICES_PER_SUBSCRIPTION: i64 = 15;

    /// Overrides from `subscription_limits` win over the deployment defaults;
    /// a zero or missing override means "use the default".
    pub fn resolve(
        defaults: Limits,
        per_device: Option<i64>,
        per_subscription: Option<i64>,
    ) -> Self {
        Self {
            per_device: per_device.filter(|v| *v > 0).unwrap_or(defaults.per_device),
            per_subscription: per_subscription
                .filter(|v| *v > 0)
                .unwrap_or(defaults.per_subscription),
            devices_per_subscription: defaults.devices_per_subscription,
        }
    }

    pub fn check_device(&self, live: i64) -> Result<(), Refusal> {
        if live >= self.per_device {
            return Err(Refusal::DeviceLimit(self.per_device));
        }
        Ok(())
    }

    pub fn check_subscription(&self, live: i64) -> Result<(), Refusal> {
        if live >= self.per_subscription {
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
    DeviceLimit(i64),
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
            Refusal::DeviceLimit(_)
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
            Refusal::DeviceLimit(_) => "device_limit",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_kind_parses_and_round_trips() {
        for kind in [ShardKind::Core, ShardKind::Premium, ShardKind::Personal] {
            assert_eq!(ShardKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ShardKind::parse(" CORE "), Some(ShardKind::Core));
        assert_eq!(ShardKind::parse("other"), None);
        // A shard without a declared kind is a region we operate.
        assert_eq!(ShardKind::default(), ShardKind::Core);
    }
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()
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
        assert!(limits.check_device(2).is_ok());
        assert_eq!(limits.check_device(3), Err(Refusal::DeviceLimit(3)));
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
        assert!(limits.check_device(2).is_ok());
        assert_eq!(limits.check_device(3), Err(Refusal::DeviceLimit(3)));
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
        assert_eq!(Refusal::DeviceLimit(5).status(), 409);
        assert_eq!(Refusal::DevicesLimit(15).code(), "devices_limit");
        assert_eq!(Refusal::GrantExpired.status(), 402);
        assert_eq!(Refusal::NoGrant.status(), 403);
        assert_eq!(Refusal::NoFreeAddress.status(), 503);
        assert_eq!(Refusal::BadPublicKey.status(), 400);
    }
}
