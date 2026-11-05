use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::net::Ipv4Addr;
use std::str::FromStr;

use std::collections::HashSet;

use fcore::{Env, Error, Inbound, Node, NodeStatus, NodeType, Tag};

/// Normalizes an optional `pool_translation` map received from a node (or a
/// third-party tool): trims keys/values, lowercases the language code,
/// aliases `ua` to `uk` and drops blank entries. An empty result becomes
/// `None`, so a partial payload can never wipe stored translations with
/// empty strings (see the COALESCE in the upsert).
fn normalize_pool_translation(
    map: Option<BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    let cleaned: BTreeMap<String, String> = map?
        .into_iter()
        .filter_map(|(key, value)| {
            let mut key = key.trim().to_lowercase();
            if key == "ua" {
                key = "uk".to_string();
            }
            let value = value.trim().to_string();
            if key.is_empty() || value.is_empty() {
                None
            } else {
                Some((key, value))
            }
        })
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum TagReq {
    #[serde(alias = "xray", alias = "Xray")]
    Xray,

    #[serde(
        alias = "allproxy",
        alias = "Allproxy",
        alias = "AllProxy",
        alias = "proxy",
        alias = "Proxy"
    )]
    Proxy,

    #[serde(alias = "wireguard", alias = "Wireguard")]
    Wireguard,

    #[serde(alias = "amneziawg", alias = "Amneziawg")]
    AmneziaWg,

    #[serde(alias = "VlessTcpReality")]
    VlessTcpReality,

    #[serde(alias = "VlessGrpcReality")]
    VlessGrpcReality,

    #[serde(alias = "VlessXhttpReality")]
    VlessXhttpReality,

    #[serde(alias = "VlessXhttpCdn")]
    VlessXhttpCdn,

    #[serde(alias = "hysteria2", alias = "Hysteria2")]
    Hysteria2,

    #[serde(alias = "mtproto", alias = "Mtproto")]
    Mtproto,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Hash, Eq)]
pub enum FormatReq {
    #[serde(alias = "Base64", alias = "base64")]
    Base64,

    #[serde(alias = "Txt", alias = "txt")]
    Txt,

    #[serde(alias = "Clash", alias = "clash")]
    Clash,
}

impl TagReq {
    pub fn tags(&self) -> Vec<Tag> {
        match self {
            TagReq::Xray => vec![
                Tag::VlessTcpReality,
                Tag::VlessGrpcReality,
                Tag::VlessXhttpReality,
                Tag::VlessXhttpCdn,
                Tag::Vmess,
                Tag::Shadowsocks,
            ],
            TagReq::Proxy => vec![
                Tag::VlessTcpReality,
                Tag::VlessGrpcReality,
                Tag::VlessXhttpReality,
                Tag::VlessXhttpCdn,
                Tag::Vmess,
                Tag::Shadowsocks,
                Tag::Hysteria2,
            ],
            TagReq::Wireguard => vec![Tag::Wireguard],
            // Mobile AWG connections are served as plain "amneziawg" links.
            TagReq::AmneziaWg => vec![Tag::AmneziaWg, Tag::AmneziaWgMobile],
            TagReq::Hysteria2 => vec![Tag::Hysteria2],
            TagReq::VlessTcpReality => vec![Tag::VlessTcpReality],
            TagReq::VlessGrpcReality => vec![Tag::VlessGrpcReality],
            TagReq::VlessXhttpReality => vec![Tag::VlessXhttpReality],
            TagReq::VlessXhttpCdn => vec![Tag::VlessXhttpCdn],
            TagReq::Mtproto => vec![Tag::Mtproto],
        }
    }

    /// The feed proto selector covering exactly this connection tag — used
    /// for the scoped per-share feed, where the client cannot pick a proto.
    pub fn for_tag(tag: Tag) -> TagReq {
        match tag {
            Tag::VlessTcpReality => TagReq::VlessTcpReality,
            Tag::VlessGrpcReality => TagReq::VlessGrpcReality,
            Tag::VlessXhttpReality => TagReq::VlessXhttpReality,
            Tag::VlessXhttpCdn => TagReq::VlessXhttpCdn,
            // No dedicated selectors for these — the xray group covers them.
            Tag::Vmess | Tag::Shadowsocks => TagReq::Xray,
            Tag::Hysteria2 => TagReq::Hysteria2,
            Tag::Wireguard => TagReq::Wireguard,
            Tag::AmneziaWg | Tag::AmneziaWgMobile => TagReq::AmneziaWg,
            Tag::Mtproto => TagReq::Mtproto,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Subscription {
    pub refer_code: Option<String>,
    pub days: Option<i64>,
    pub limit_bytes: Option<i64>,
}

impl Subscription {
    pub fn validate(&self) -> Result<(), String> {
        match self.days {
            Some(days) if days > 0 => Ok(()),
            Some(_) => Err("days must be greater than 0".to_string()),
            None => Err("days is required".to_string()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeRequest {
    pub env: Env,
    pub hostname: String,
    pub address: Ipv4Addr,
    pub inbounds: HashMap<Tag, Inbound>,
    pub uuid: uuid::Uuid,
    pub label: String,
    pub interface: String,
    pub cores: usize,
    pub max_bandwidth_bps: i64,
    pub country: String,
    pub r#type: Option<NodeType>,
    #[serde(default)]
    pub cluster: Option<String>,
    /// Extra entry IPs sent by the node at registration: node_ips[0] is the
    /// primary and must equal `address`; absent = single-address node
    /// (old node binaries simply omit the field).
    #[serde(default)]
    pub node_ips: Option<Vec<Ipv4Addr>>,
    /// Optional pool/group name (config.toml `[node] pool`). Old node
    /// binaries omit it, so old clients keep the previous behavior.
    #[serde(default)]
    pub pool: Option<String>,
    /// Per-entry weights (config.toml `[node] node_weights`), keyed by address.
    /// Old node binaries omit the field (and send the legacy `weight`, which the
    /// api ignores), so every address falls back to the default (100).
    #[serde(default)]
    pub node_weights: BTreeMap<String, i32>,
    /// Optional per-language translations of `pool` (en/ru/uk/...); `ua` is
    /// accepted as an alias of `uk`.
    #[serde(default)]
    pub pool_translation: Option<BTreeMap<String, String>>,
}

impl NodeRequest {
    /// Consistency rule for the entry-IP list: when present it must be
    /// non-empty and start with the primary address, so old clients that
    /// only read `address` stay correct.
    pub fn validate(&self) -> Result<(), Error> {
        if let Some(node_ips) = &self.node_ips {
            match node_ips.first() {
                None => {
                    return Err(Error::Custom(
                        "node_ips must be non-empty when present".into(),
                    ));
                }
                Some(primary) if *primary != self.address => {
                    return Err(Error::Custom(
                        "node_ips[0] must equal the node address".into(),
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub fn as_node(&self) -> Node {
        let now = Utc::now();

        let t = if let Some(t) = self.r#type {
            t
        } else {
            NodeType::Node
        };
        Node {
            uuid: self.uuid,
            env: self.env.clone(),
            hostname: self.hostname.clone(),
            address: self.address,
            inbounds: self.inbounds.clone(),
            status: NodeStatus::Online,
            created_at: now,
            modified_at: now,
            label: self.label.clone(),
            interface: self.interface.clone(),
            cores: self.cores,
            max_bandwidth_bps: self.max_bandwidth_bps,
            country: self.country.clone(),
            r#type: t,
            cluster: self.cluster.clone(),
            node_ips: self.node_ips.clone(),
            pool: self
                .pool
                .clone()
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty()),
            pool_translation: normalize_pool_translation(self.pool_translation.clone()),
            node_weights: self
                .node_weights
                .iter()
                .filter(|(ip, w)| **w > 0 && ip.parse::<Ipv4Addr>().is_ok())
                .map(|(ip, w)| (ip.trim().to_string(), *w))
                .collect(),
        }
    }
}

pub(crate) fn normalized_pool(pool: Option<String>) -> Option<String> {
    pool.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// One client-facing field that differs between two nodes of the same pool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PoolFieldDiff {
    pub tag: String,
    pub field: String,
    pub peer: serde_json::Value,
    pub incoming: serde_json::Value,
}

/// Why a node does not fit the pool its config claims.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PoolConflict {
    pub pool: String,
    pub peer_hostname: String,
    pub differences: Vec<PoolFieldDiff>,
}

/// Short, non-reversible fingerprint of a private key: enough to say "same key"
/// or "different key" in a 409 body without ever leaking the secret.
fn key_fingerprint(privkey: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(privkey.as_bytes());
    format!("sha256:{}", hex::encode(&digest[..4]))
}

/// Client-facing fingerprint of an inbound.
///
/// Only fields that end up in a client config are compared. Local interface
/// names (`awg0` vs `awg1`), node ids and timestamps legitimately differ
/// between nodes of one pool, and map order must never matter — comparing whole
/// `Inbound` values is what produced the endless 409 `pool_key_mismatch`.
fn client_inbound_fields(inbound: &Inbound) -> BTreeMap<String, serde_json::Value> {
    let mut out: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    out.insert("port".to_string(), serde_json::json!(inbound.port));

    if let Some(wg) = &inbound.wg {
        out.insert(
            "address".to_string(),
            serde_json::json!(wg.address.to_string()),
        );
        out.insert("dns".to_string(), serde_json::json!(wg.dns));
        out.insert("keepalive".to_string(), serde_json::json!(wg.keepalive));
        out.insert(
            "public_key".to_string(),
            serde_json::json!(key_fingerprint(&wg.keys.privkey)),
        );
    }

    if let Some(awg) = &inbound.awg {
        out.insert(
            "address".to_string(),
            serde_json::json!(awg.interface.address.to_string()),
        );
        out.insert(
            "listen_port".to_string(),
            serde_json::json!(awg.interface.listen_port),
        );
        out.insert("mtu".to_string(), serde_json::json!(awg.interface.mtu));
        out.insert("dns".to_string(), serde_json::json!(awg.interface.dns));
        out.insert("keepalive".to_string(), serde_json::json!(awg.keepalive));
        out.insert(
            "public_key".to_string(),
            serde_json::json!(key_fingerprint(&awg.interface.private_key.privkey)),
        );

        match &awg.obfuscation {
            Some(o) => {
                // Flatten the whole profile: every parameter is part of the
                // handshake, and new ones (AWG 3.0) must be covered without
                // another edit here. `random_trailers`/`disable_cookies` are
                // `skip_serializing_if = None`, so they appear only when set —
                // that is fine, absence is compared as absence.
                if let Ok(serde_json::Value::Object(map)) = serde_json::to_value(o) {
                    for (key, value) in map {
                        out.insert(format!("awg_obfuscation.{key}"), value);
                    }
                }
            }
            None => {
                out.insert("awg_obfuscation".to_string(), serde_json::json!(null));
            }
        }
    }

    if let Some(h2) = &inbound.h2 {
        out.insert("sni".to_string(), serde_json::json!(h2.sni));
    }

    if let Some(stream) = &inbound.stream_settings {
        out.insert(
            "stream_settings".to_string(),
            serde_json::to_value(stream).unwrap_or(serde_json::Value::Null),
        );
    }

    out
}

/// Compares the incoming node's inbounds with its pool peers.
///
/// `None` means the node fits — or that it has no pool at all: a standalone node
/// is never compared with anything. The comparison is over a `BTreeMap<tag, …>`,
/// so neither the order of inbounds nor local interface names can create a
/// difference on their own.
pub(crate) fn pool_inbounds_conflict(peers: &[Node], incoming: &Node) -> Option<PoolConflict> {
    let pool = normalized_pool(incoming.pool.clone())?;

    let incoming_fields: BTreeMap<String, BTreeMap<String, serde_json::Value>> = incoming
        .inbounds
        .iter()
        .map(|(tag, inbound)| (tag.to_string(), client_inbound_fields(inbound)))
        .collect();

    let mut peers: Vec<&Node> = peers
        .iter()
        .filter(|n| {
            n.uuid != incoming.uuid
                && n.status.is_serving()
                && normalized_pool(n.pool.clone()).as_deref() == Some(pool.as_str())
        })
        .collect();
    // Deterministic diagnostics: the same mismatch must name the same peer.
    peers.sort_by_key(|n| n.uuid);

    for peer in peers {
        let peer_fields: BTreeMap<String, BTreeMap<String, serde_json::Value>> = peer
            .inbounds
            .iter()
            .map(|(tag, inbound)| (tag.to_string(), client_inbound_fields(inbound)))
            .collect();

        let mut differences = Vec::new();

        for (tag, peer_map) in &peer_fields {
            match incoming_fields.get(tag) {
                None => differences.push(PoolFieldDiff {
                    tag: tag.clone(),
                    field: "inbounds".to_string(),
                    peer: serde_json::json!("present"),
                    incoming: serde_json::json!("missing"),
                }),
                Some(incoming_map) => {
                    for (field, peer_value) in peer_map {
                        let incoming_value = incoming_map
                            .get(field)
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        if &incoming_value != peer_value {
                            differences.push(PoolFieldDiff {
                                tag: tag.clone(),
                                field: field.clone(),
                                peer: peer_value.clone(),
                                incoming: incoming_value,
                            });
                        }
                    }
                }
            }
        }

        for tag in incoming_fields.keys() {
            if !peer_fields.contains_key(tag) {
                differences.push(PoolFieldDiff {
                    tag: tag.clone(),
                    field: "inbounds".to_string(),
                    peer: serde_json::json!("missing"),
                    incoming: serde_json::json!("present"),
                });
            }
        }

        if !differences.is_empty() {
            return Some(PoolConflict {
                pool,
                peer_hostname: peer.hostname.clone(),
                differences,
            });
        }
    }

    None
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ConnCreateRequest {
    pub env: Env,
    pub subscription_id: Option<uuid::Uuid>,
    pub proto: Tag,
    pub days: Option<u16>,
    /// Optional user-facing name for a "named device" connection
    /// (e.g. "Мама Андроид"). None = system/default connection.
    #[serde(default)]
    pub label: Option<String>,
    /// Optional pin of a named-device connection to a single node
    /// (nodes.uuid): the peer exists only on that node. None = env-wide
    /// (current behavior).
    #[serde(default)]
    pub node_id: Option<uuid::Uuid>,
    /// Missing = separate-link-only for a new named device. Defaults stay included.
    #[serde(default)]
    pub include_in_main: Option<bool>,
}

impl ConnCreateRequest {
    /// Trimmed label, or None when it is absent or blank. Used both for
    /// validation and for persistence so the two never disagree.
    pub fn normalized_label(&self) -> Option<String> {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if let Some(label) = self.normalized_label() {
            // A non-empty trimmed label is at least 1 char, so only the
            // upper bound can fail here. Counted in chars, not bytes:
            // labels are user-facing names and may be Cyrillic.
            if label.chars().count() > 64 {
                return Err(Error::Custom("label must be 1..=64 characters".into()));
            }
        }

        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
pub struct KeyReq {
    pub days: i16,
    pub distributor: Option<String>,
    /// Key kind: absent/"standard" keeps the v1 days behavior, "lite"
    /// creates a traffic-only key and requires traffic_gib.
    pub kind: Option<String>,
    pub traffic_gib: Option<u32>,
    /// Tariff product this key sells (dev, gaming, bs-prem, dev-gaming,
    /// dev-bs-prem, full). Absent = legacy: the historical access from
    /// enabled_conns. New products are standard (unlimited) keys only.
    pub product: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddTrafficReq {
    /// Bytes to add to the subscription limit; must be positive.
    pub add_bytes: i64,
    /// Idempotency key: a repeated top-up with the same trace_id is a no-op.
    pub trace_id: uuid::Uuid,
}

#[derive(Serialize, Deserialize)]
pub struct ActivateKeyReq {
    pub code: String,
    pub subscription_id: Option<uuid::Uuid>,
    pub limit_bytes: Option<i64>,
    /// Optional for lite keys: when present, fcore binds it to the
    /// subscription via mrkting's POST /account. Ignored for standard keys.
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnvFilter {
    Single(Env),
    All,
}

impl<'de> Deserialize<'de> for EnvFilter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?.to_lowercase();

        match s.as_str() {
            "all" => Ok(EnvFilter::All),
            _ => {
                let env = Env::from_str(&s).map_err(serde::de::Error::custom)?;
                Ok(EnvFilter::Single(env))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SubscriptionInfoRequest {
    pub id: uuid::Uuid,
    pub format: FormatReq,
    pub env: EnvFilter,
    pub proto: TagReq,
    #[serde(default)]
    pub app: Option<String>,
    /// Optional single-connection (named device) scope: the feed contains
    /// only links of this connection. The connection must belong to the
    /// subscription, otherwise the handler answers 404.
    #[serde(default)]
    pub conn: Option<uuid::Uuid>,
}

fn default_share_feed_format() -> FormatReq {
    FormatReq::Base64
}

/// Query for the public per-share feed (GET /sub/<token>): the same knobs
/// as the subscription feed minus id/env/proto, which are pinned by the
/// token. Unlike SubscriptionInfoRequest, `format` is optional —
/// third-party clients (Happ/Streisand) fetch the bare URL.
#[derive(Debug, Deserialize)]
pub struct ShareFeedQuery {
    #[serde(default = "default_share_feed_format")]
    pub format: FormatReq,
    #[serde(default)]
    pub app: Option<String>,
}

impl SubscriptionInfoRequest {
    fn allowed_formats(&self, proto: &TagReq) -> HashSet<FormatReq> {
        use FormatReq::*;
        use TagReq::*;

        match proto {
            Xray => [Txt, Base64, Clash].into(),
            Proxy => [Txt, Base64, Clash].into(),
            Wireguard => [].into(),
            AmneziaWg => [].into(),
            Hysteria2 => [Txt, Base64, Clash].into(),
            VlessTcpReality => [Txt, Base64, Clash].into(),
            VlessGrpcReality => [Txt, Base64, Clash].into(),
            VlessXhttpReality => [Txt, Base64, Clash].into(),
            VlessXhttpCdn => [Txt, Base64, Clash].into(),
            Mtproto => [].into(),
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        let allowed = self.allowed_formats(&self.proto);

        if !allowed.contains(&self.format) {
            return Err(Error::Custom(format!(
                "Format {:?} not allowed for proto {:?}",
                self.format, self.proto
            )));
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct RefCodeQuery {
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct ConnectionInfoRequest {
    pub id: uuid::Uuid,
    pub env: Env,
    /// Optional single-connection (named device) scope: only this
    /// connection's configs are returned.
    #[serde(default)]
    pub conn: Option<uuid::Uuid>,
}

impl ConnectionInfoRequest {
    pub fn validate(&self) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_req_standard_without_kind() {
        let req: KeyReq = serde_json::from_str(r#"{"days": 30}"#).unwrap();

        assert_eq!(req.days, 30);
        assert!(req.kind.is_none());
        assert!(req.traffic_gib.is_none());
    }

    #[test]
    fn test_key_req_lite() {
        let req: KeyReq =
            serde_json::from_str(r#"{"days": 0, "kind": "lite", "traffic_gib": 5}"#).unwrap();

        assert_eq!(req.kind.as_deref(), Some("lite"));
        assert_eq!(req.traffic_gib, Some(5));
    }

    #[test]
    fn test_key_req_requires_days() {
        let parsed: Result<KeyReq, _> = serde_json::from_str(r#"{"kind": "lite"}"#);

        assert!(parsed.is_err());
    }

    #[test]
    fn test_activate_key_req_email_optional() {
        let req: ActivateKeyReq = serde_json::from_str(r#"{"code": "XXXX"}"#).unwrap();
        assert!(req.email.is_none());

        let req: ActivateKeyReq =
            serde_json::from_str(r#"{"code": "XXXX", "email": "a@b.c"}"#).unwrap();
        assert_eq!(req.email.as_deref(), Some("a@b.c"));
    }

    fn req_with_label(label: Option<&str>) -> ConnCreateRequest {
        ConnCreateRequest {
            env: Env::Ru,
            subscription_id: None,
            proto: Tag::Wireguard,
            days: None,
            label: label.map(str::to_string),
            node_id: None,
            include_in_main: None,
        }
    }

    #[test]
    fn test_label_absent_is_valid() {
        let req = req_with_label(None);
        assert!(req.validate().is_ok());
        assert_eq!(req.normalized_label(), None);
        assert_eq!(req.include_in_main, None);
    }

    #[test]
    fn test_conn_create_include_in_main_parses() {
        let req: ConnCreateRequest = serde_json::from_str(
            r#"{"env":"ru","proto":"Wireguard","label":"Phone","include_in_main":true}"#,
        )
        .unwrap();
        assert_eq!(req.include_in_main, Some(true));
    }

    #[test]
    fn test_label_blank_becomes_none() {
        for blank in ["", "   ", "\t \n"] {
            let req = req_with_label(Some(blank));
            assert!(req.validate().is_ok());
            assert_eq!(req.normalized_label(), None);
        }
    }

    #[test]
    fn test_label_is_trimmed() {
        let req = req_with_label(Some("  Мама Андроид  "));
        assert!(req.validate().is_ok());
        assert_eq!(req.normalized_label().as_deref(), Some("Мама Андроид"));
    }

    #[test]
    fn test_label_max_length_ok() {
        let label = "я".repeat(64);
        let req = req_with_label(Some(&label));
        assert!(req.validate().is_ok());
        assert_eq!(req.normalized_label().as_deref(), Some(label.as_str()));
    }

    #[test]
    fn test_label_too_long_rejected() {
        // 65 Cyrillic chars = 130 bytes: the limit counts characters.
        let label = "я".repeat(65);
        let req = req_with_label(Some(&label));
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_subscription_info_conn_param() {
        let id = uuid::Uuid::new_v4();
        let conn = uuid::Uuid::new_v4();

        let qs = format!("id={}&format=txt&env=all&proto=proxy&conn={}", id, conn);
        let req: SubscriptionInfoRequest = serde_urlencoded::from_str(&qs).unwrap();
        assert_eq!(req.id, id);
        assert_eq!(req.conn, Some(conn));

        // Absent conn — whole-subscription feed, as before.
        let qs = format!("id={}&format=txt&env=all&proto=proxy", id);
        let req: SubscriptionInfoRequest = serde_urlencoded::from_str(&qs).unwrap();
        assert_eq!(req.conn, None);
    }

    #[test]
    fn test_clash_allowed_for_hysteria2_and_proxy() {
        let id = uuid::Uuid::new_v4();

        for proto in ["hysteria2", "proxy"] {
            let qs = format!("id={}&format=clash&env=all&proto={}", id, proto);
            let req: SubscriptionInfoRequest = serde_urlencoded::from_str(&qs).unwrap();
            assert!(req.validate().is_ok(), "clash rejected for proto={proto}");
        }

        // Sanity: protocols without a clash renderer stay rejected.
        let qs = format!("id={}&format=clash&env=all&proto=wireguard", id);
        let req: SubscriptionInfoRequest = serde_urlencoded::from_str(&qs).unwrap();
        assert!(req.validate().is_err());
    }

    fn node_request_json(extra: &str) -> serde_json::Value {
        let mut v = serde_json::json!({
            "env": "ru",
            "hostname": "node-1",
            "address": "78.17.28.66",
            "inbounds": {},
            "uuid": uuid::Uuid::new_v4(),
            "label": "Node",
            "interface": "eth0",
            "cores": 4,
            "max_bandwidth_bps": 1000000000i64,
            "country": "RU",
            "type": "Node"
        });
        if !extra.is_empty() {
            let extra: serde_json::Value = serde_json::from_str(extra).unwrap();
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
        }
        v
    }

    #[test]
    fn test_node_request_without_node_ips_backward_compat() {
        // Old node binaries send no node_ips field at all.
        let req: NodeRequest = serde_json::from_value(node_request_json("")).unwrap();
        assert!(req.node_ips.is_none());
        assert!(req.validate().is_ok());
        assert!(req.as_node().node_ips.is_none());
    }

    #[test]
    fn test_node_request_node_ips_passthrough() {
        let req: NodeRequest = serde_json::from_value(node_request_json(
            r#"{"node_ips": ["78.17.28.66", "78.17.28.67"]}"#,
        ))
        .unwrap();
        assert!(req.validate().is_ok());
        let node = req.as_node();
        assert_eq!(
            node.node_ips,
            Some(vec![
                "78.17.28.66".parse::<Ipv4Addr>().unwrap(),
                "78.17.28.67".parse::<Ipv4Addr>().unwrap(),
            ])
        );
    }

    #[test]
    fn test_node_request_node_ips_primary_mismatch_rejected() {
        let req: NodeRequest = serde_json::from_value(node_request_json(
            r#"{"node_ips": ["78.17.28.67", "78.17.28.66"]}"#,
        ))
        .unwrap();
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_node_request_node_ips_empty_rejected() {
        let req: NodeRequest =
            serde_json::from_value(node_request_json(r#"{"node_ips": []}"#)).unwrap();
        assert!(req.validate().is_err());
    }

    fn node_with_port(port: u16, pool: Option<&str>, id: &str) -> Node {
        let mut req: NodeRequest = serde_json::from_value(node_request_json("")).unwrap();
        req.uuid = uuid::Uuid::parse_str(id).unwrap();
        req.pool = pool.map(|s| s.to_string());
        let mut node = req.as_node();
        node.inbounds.insert(
            Tag::VlessTcpReality,
            Inbound {
                tag: Tag::VlessTcpReality,
                port,
                stream_settings: None,
                wg: None,
                awg: None,
                h2: None,
                mtproto_secret: None,
            },
        );
        node
    }

    /// AmneziaWG inbound with the fields a client actually receives.
    fn awg_inbound(port: u16, privkey: &str, jc: u16, iface: &str) -> Inbound {
        Inbound {
            tag: Tag::AmneziaWgMobile,
            port,
            stream_settings: None,
            wg: None,
            awg: Some(fcore::AmneziaWgSettings {
                interface: fcore::AwgInterfaceConfig {
                    interface: iface.to_string(),
                    address: "10.77.0.1/16".parse().unwrap(),
                    listen_port: port,
                    mtu: Some(1280),
                    private_key: fcore::WgKeys {
                        privkey: privkey.to_string(),
                    },
                    dns: vec!["1.1.1.1".parse().unwrap()],
                },
                obfuscation: Some(fcore::AwgObfuscationParams {
                    jc,
                    jmin: 10,
                    jmax: 50,
                    s1: 1,
                    s2: 2,
                    s3: 3,
                    s4: 4,
                    h1: "1".into(),
                    h2: "2".into(),
                    h3: "3".into(),
                    h4: "4".into(),
                    i1: "".into(),
                    i2: "".into(),
                    i3: "".into(),
                    i4: "".into(),
                    i5: "".into(),
                    random_trailers: None,
                    disable_cookies: None,
                    header_protection_key: None,
                    content_padding_addition: None,
                    rekey_after_time: None,
                    rekey_timeout: None,
                    reject_after_time: None,
                    keepalive_timeout: None,
                    max_handshake_attempts: None,
                }),
                keepalive: Some(25),
            }),
            h2: None,
            mtproto_secret: None,
        }
    }

    fn pool_node(id: &str, with_awg: Option<Inbound>) -> Node {
        let mut node = node_with_port(443, Some("RU"), id);
        if let Some(inbound) = with_awg {
            node.inbounds.insert(inbound.tag, inbound);
        }
        node
    }

    #[test]
    fn pool_inbounds_same_set_in_different_order_is_not_a_conflict() {
        // The 409 was endless because whole structures were compared: two nodes
        // with the same protocols but a different map order differ as values.
        let a = pool_node(
            "11111111-1111-1111-1111-111111111111",
            Some(awg_inbound(443, "key-a", 5, "awg0")),
        );
        let mut b = node_with_port(443, Some("RU"), "22222222-2222-2222-2222-222222222222");
        // Insert the same two inbounds in the opposite order.
        b.inbounds
            .insert(Tag::AmneziaWgMobile, awg_inbound(443, "key-a", 5, "awg1"));
        b.inbounds.insert(
            Tag::VlessTcpReality,
            Inbound {
                tag: Tag::VlessTcpReality,
                port: 443,
                stream_settings: None,
                wg: None,
                awg: None,
                h2: None,
                mtproto_secret: None,
            },
        );

        assert_eq!(pool_inbounds_conflict(&[a], &b), None);
    }

    #[test]
    fn pool_inbounds_interface_name_alone_is_not_a_conflict() {
        // Local interface names legitimately differ between hosts.
        let a = pool_node(
            "11111111-1111-1111-1111-111111111111",
            Some(awg_inbound(443, "key-a", 5, "awg0")),
        );
        let b = pool_node(
            "22222222-2222-2222-2222-222222222222",
            Some(awg_inbound(443, "key-a", 5, "awg1")),
        );
        assert_eq!(pool_inbounds_conflict(&[a], &b), None);
    }

    #[test]
    fn pool_inbounds_port_mismatch_names_the_field() {
        let a = pool_node(
            "11111111-1111-1111-1111-111111111111",
            Some(awg_inbound(443, "key-a", 5, "awg0")),
        );
        let b = pool_node(
            "22222222-2222-2222-2222-222222222222",
            Some(awg_inbound(8443, "key-a", 5, "awg0")),
        );
        let conflict = pool_inbounds_conflict(&[a], &b).expect("port must conflict");
        let port = conflict
            .differences
            .iter()
            .find(|d| d.field == "port")
            .expect("port difference must be reported");
        assert_eq!(port.tag, "AmneziaWgMobile");
        assert_eq!(port.peer, serde_json::json!(443));
        assert_eq!(port.incoming, serde_json::json!(8443));
    }

    #[test]
    fn pool_inbounds_key_mismatch_is_reported_without_leaking_the_key() {
        let a = pool_node(
            "11111111-1111-1111-1111-111111111111",
            Some(awg_inbound(443, "super-secret-a", 5, "awg0")),
        );
        let b = pool_node(
            "22222222-2222-2222-2222-222222222222",
            Some(awg_inbound(443, "super-secret-b", 5, "awg0")),
        );
        let conflict = pool_inbounds_conflict(&[a], &b).expect("key must conflict");
        let key = conflict
            .differences
            .iter()
            .find(|d| d.field == "public_key")
            .expect("key difference must be reported");
        let rendered = serde_json::to_string(&conflict).unwrap();
        assert!(!rendered.contains("super-secret"), "keys must never be echoed");
        assert!(key.peer.as_str().unwrap().starts_with("sha256:"));
    }

    #[test]
    fn pool_inbounds_obfuscation_mismatch_names_the_exact_parameter() {
        // A different jc means the client cannot complete a handshake.
        let a = pool_node(
            "11111111-1111-1111-1111-111111111111",
            Some(awg_inbound(443, "key-a", 5, "awg0")),
        );
        let b = pool_node(
            "22222222-2222-2222-2222-222222222222",
            Some(awg_inbound(443, "key-a", 6, "awg0")),
        );
        let conflict = pool_inbounds_conflict(&[a], &b).expect("obfuscation must conflict");
        assert_eq!(conflict.differences.len(), 1);
        assert_eq!(conflict.differences[0].field, "awg_obfuscation.jc");
        assert_eq!(conflict.differences[0].peer, serde_json::json!(5));
        assert_eq!(conflict.differences[0].incoming, serde_json::json!(6));
    }

    #[test]
    fn pool_inbounds_missing_tag_is_reported_as_inbounds_difference() {
        let a = pool_node(
            "11111111-1111-1111-1111-111111111111",
            Some(awg_inbound(443, "key-a", 5, "awg0")),
        );
        let b = node_with_port(443, Some("RU"), "22222222-2222-2222-2222-222222222222");
        let conflict = pool_inbounds_conflict(&[a], &b).expect("missing tag must conflict");
        let missing = conflict
            .differences
            .iter()
            .find(|d| d.tag == "AmneziaWgMobile")
            .expect("missing inbound must be reported");
        assert_eq!(missing.field, "inbounds");
        assert_eq!(missing.peer, serde_json::json!("present"));
        assert_eq!(missing.incoming, serde_json::json!("missing"));
    }

    #[test]
    fn pool_check_is_skipped_without_pool_and_across_pools() {
        // No pool: a standalone node is never compared.
        let solo = node_with_port(443, None, "11111111-1111-1111-1111-111111111111");
        let other = node_with_port(8443, None, "22222222-2222-2222-2222-222222222222");
        assert_eq!(pool_inbounds_conflict(&[solo], &other), None);

        // Same node ids but different pools: also not compared.
        let a = pool_node(
            "11111111-1111-1111-1111-111111111111",
            Some(awg_inbound(443, "key-a", 5, "awg0")),
        );
        let mut b = pool_node(
            "22222222-2222-2222-2222-222222222222",
            Some(awg_inbound(443, "key-b", 9, "awg0")),
        );
        b.pool = Some("EU".to_string());
        assert_eq!(pool_inbounds_conflict(&[a], &b), None);
    }

    #[test]
    fn test_pool_blank_is_absent_and_keys_must_match() {
        let req: NodeRequest =
            serde_json::from_value(node_request_json(r#"{"pool":"  "}"#)).unwrap();
        assert!(req.as_node().pool.is_none());

        let a = node_with_port(443, Some("EU"), "11111111-1111-1111-1111-111111111111");
        let same = node_with_port(443, Some("EU"), "22222222-2222-2222-2222-222222222222");
        let other = node_with_port(8443, Some("EU"), "22222222-2222-2222-2222-222222222222");
        let solo = node_with_port(8443, Some("EU"), "11111111-1111-1111-1111-111111111111");
        assert_eq!(pool_inbounds_conflict(&[a.clone()], &same), None);
        assert!(pool_inbounds_conflict(&[a.clone()], &other).is_some());
        assert_eq!(pool_inbounds_conflict(&[a], &solo), None);
    }
}
