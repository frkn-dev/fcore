use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::str::FromStr;

use std::collections::HashSet;

use fcore::{Env, Error, Inbound, Node, NodeStatus, NodeType, Tag};

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
}

impl NodeRequest {
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
        }
    }
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
}

#[derive(Serialize, Deserialize)]
pub struct ActivateKeyReq {
    pub code: String,
    pub subscription_id: Option<uuid::Uuid>,
    pub limit_bytes: Option<i64>,
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
            Proxy => [Txt, Base64].into(),
            Wireguard => [].into(),
            AmneziaWg => [].into(),
            Hysteria2 => [Txt, Base64].into(),
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

    fn req_with_label(label: Option<&str>) -> ConnCreateRequest {
        ConnCreateRequest {
            env: Env::Ru,
            subscription_id: None,
            proto: Tag::Wireguard,
            days: None,
            label: label.map(str::to_string),
            node_id: None,
        }
    }

    #[test]
    fn test_label_absent_is_valid() {
        let req = req_with_label(None);
        assert!(req.validate().is_ok());
        assert_eq!(req.normalized_label(), None);
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
}
