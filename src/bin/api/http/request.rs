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
            TagReq::AmneziaWg => vec![Tag::AmneziaWg],
            TagReq::Hysteria2 => vec![Tag::Hysteria2],
            TagReq::VlessTcpReality => vec![Tag::VlessTcpReality],
            TagReq::VlessGrpcReality => vec![Tag::VlessGrpcReality],
            TagReq::VlessXhttpReality => vec![Tag::VlessXhttpReality],
            TagReq::VlessXhttpCdn => vec![Tag::VlessXhttpCdn],
            TagReq::Mtproto => vec![Tag::Mtproto],
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
}

impl ConnCreateRequest {
    pub fn validate(&self) -> Result<(), Error> {
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
}

impl ConnectionInfoRequest {
    pub fn validate(&self) -> Result<(), Error> {
        Ok(())
    }
}
