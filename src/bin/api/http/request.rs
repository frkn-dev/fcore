use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::Ipv4Addr;

use std::collections::HashSet;

use fcore::{Env, Error, Inbound, Node, NodeStatus, NodeType, Tag};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum TagReq {
    #[serde(alias = "xray", alias = "Xray")]
    Xray,

    #[serde(alias = "wireguard", alias = "Wireguard")]
    Wireguard,

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
                Tag::Vmess,
                Tag::Shadowsocks,
            ],
            TagReq::Wireguard => vec![Tag::Wireguard],
            TagReq::Hysteria2 => vec![Tag::Hysteria2],
            TagReq::Mtproto => vec![Tag::Mtproto],
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Subscription {
    pub referred_by: Option<String>,
    pub refer_code: Option<String>,
    pub days: Option<i64>,
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
}

impl NodeRequest {
    pub fn as_node(&self) -> Node {
        let now = Utc::now();

        let t = if let Some(t) = self.r#type {
            t
        } else {
            NodeType::Common
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
    pub subscription_id: uuid::Uuid,
}

#[derive(Debug, Deserialize)]
pub struct SubscriptionInfoRequest {
    pub id: uuid::Uuid,
    pub format: FormatReq,
    pub env: Env,
    pub proto: TagReq,
}

impl SubscriptionInfoRequest {
    fn allowed_formats(&self, proto: &TagReq) -> HashSet<FormatReq> {
        use FormatReq::*;
        use TagReq::*;

        match proto {
            Xray => [Txt, Base64, Clash].into(),
            Wireguard => [].into(),
            Hysteria2 => [Txt, Base64].into(),
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
pub struct ConnectionInfoRequest {
    pub id: uuid::Uuid,
    pub env: Env,
}

impl ConnectionInfoRequest {
    pub fn validate(&self) -> Result<(), Error> {
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct Trial {
    pub user: Option<String>,
    pub email: Option<String>,
    pub referred_by: Option<String>,
}

impl Trial {
    pub fn validate(&self) -> Result<(), Error> {
        if self.user.is_some() && self.email.is_some() {
            return Err(Error::Custom("Only one param is allowed".to_string()));
        }
        Ok(())
    }
}
