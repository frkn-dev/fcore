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

    /// The kind a shard has unless the config says otherwise. `kind` is a
    /// property of the shard, not of the tariff, but for every shard we run it is
    /// fully implied by the plan — declaring it by hand was one more place to
    /// disagree with `plan`.
    pub fn for_plan(plan: &str) -> Option<Self> {
        match plan.trim().to_ascii_lowercase().as_str() {
            "standard" | "lite" => Some(Self::Core),
            "premium" => Some(Self::Premium),
            "selfhosted" => Some(Self::Personal),
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
    /// Live tunnels of one physical device, counted by its X25519 key across
    /// every subscription and env it holds. A phone keeps one; a router or a
    /// Linux box may keep three up at once for failover.
    pub per_device: i64,
    /// Tunnels of the whole subscription — the hard cap on the address budget.
    pub per_subscription: i64,
    /// Installations of the whole subscription.
    pub devices_per_subscription: i64,
}

impl Limits {
    /// One key = one device, so the cap is per device and not per subscription:
    /// a second subscription must not let the same machine open another three
    /// tunnels. Three covers the router case (three tunnels up for failover).
    pub const DEFAULT_PEERS_PER_DEVICE: i64 = 3;
    /// 15 devices × 3 tunnels: the peer cap must not bind before the device cap.
    pub const DEFAULT_PER_SUBSCRIPTION: i64 = 45;
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
