use std::fmt;
use std::str::FromStr;
use std::{
    collections::{BTreeMap, HashMap},
    net::Ipv4Addr,
};

use chrono::DateTime;
use chrono::Utc;
use postgres_types::{FromSql, ToSql};
use serde::{Deserialize, Serialize};

use super::env::Env;
use super::tag::ProtoTag as Tag;

use crate::config::h2::H2Settings;

#[cfg(feature = "xray")]
use crate::config::inbound::Settings as XraySettings;

#[cfg(feature = "amnezia-wg")]
use crate::config::amnezia_wg::AmneziaWgSettings;

use crate::config::inbound::{Inbound, InboundResponse};
use crate::config::mtproto::MtprotoSettings;
use crate::config::settings::NodeConfig;

#[cfg(feature = "wireguard")]
use crate::config::wireguard::WireguardSettings;

#[derive(Clone, Debug, Deserialize, Serialize, Copy, ToSql, FromSql)]
#[postgres(name = "node_status", rename_all = "snake_case")]
pub enum Status {
    Online,
    Offline,
}

impl PartialEq for Status {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Status::Online, Status::Online) | (Status::Offline, Status::Offline)
        )
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Online => write!(f, "Online"),
            Status::Offline => write!(f, "Offline"),
        }
    }
}

impl FromStr for Status {
    type Err = ();

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "Online" => Ok(Status::Online),
            "Offline" => Ok(Status::Offline),
            _ => Ok(Status::Offline),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Copy, ToSql, FromSql, PartialEq)]
#[postgres(name = "node_type")]
pub enum Type {
    #[postgres(name = "common")]
    Node,

    #[postgres(name = "premium")]
    PremiumNode,

    #[postgres(name = "service")]
    Service,

    #[postgres(name = "agent")]
    Agent,
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Node => write!(f, "Node"),
            Type::PremiumNode => write!(f, "PremiumNode"),
            Type::Service => write!(f, "Service"),
            Type::Agent => write!(f, "Agent"),
        }
    }
}

impl FromStr for Type {
    type Err = ();

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input.to_lowercase().as_str() {
            "agent" | "common" | "node" => Ok(Type::Node),
            "premium" | "premiumnode" | "premium_node" => Ok(Type::PremiumNode),
            "service" => Ok(Type::Service),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeResponse {
    pub uuid: uuid::Uuid,
    pub env: String,
    pub hostname: String,
    pub interface: String,
    pub address: Ipv4Addr,
    pub inbounds: Vec<InboundResponse>,
    pub status: Status,
    pub label: String,
    pub cores: usize,
    pub max_bandwidth_bps: i64,
    pub metrics: Vec<NodeMetricInfo>,
    pub country: String,
    pub r#type: Type,
    pub cluster: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct NodeMetricInfo {
    pub key: String,
    pub name: String,
    pub tags: BTreeMap<String, String>,
}

pub struct InboundStat {
    pub downlink: i64,
    pub uplink: i64,
    pub conn_count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Node {
    pub uuid: uuid::Uuid,
    pub env: Env,
    pub hostname: String,
    pub address: Ipv4Addr,
    pub status: Status,
    pub label: String,
    pub interface: String,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
    pub inbounds: HashMap<Tag, Inbound>,
    pub cores: usize,
    pub max_bandwidth_bps: i64,
    pub country: String,
    pub r#type: Type,
    pub cluster: Option<String>,
}

impl Node {
    pub fn new(
        settings: NodeConfig,
        #[cfg(feature = "xray")] xray_config: Option<XraySettings>,
        #[cfg(feature = "wireguard")] wg_config: Option<WireguardSettings>,
        #[cfg(feature = "amnezia-wg")] awg_config: Option<AmneziaWgSettings>,
        #[cfg(feature = "amnezia-wg")] awg_mobile_config: Option<AmneziaWgSettings>,
        h2_config: Option<H2Settings>,
        mtproto_config: Option<MtprotoSettings>,
    ) -> Self {
        let now = Utc::now();
        let mut inbounds: HashMap<Tag, Inbound> = HashMap::new();

        {
            #[cfg(feature = "xray")]
            if let Some(config) = xray_config {
                let xray_inbounds = config
                    .inbounds
                    .into_iter()
                    .map(|inbound| (inbound.tag, inbound))
                    .collect::<HashMap<Tag, Inbound>>();

                inbounds.extend(xray_inbounds);
            }

            #[cfg(feature = "wireguard")]
            if let Some(ref config) = wg_config {
                inbounds.insert(
                    Tag::Wireguard,
                    Inbound {
                        port: config.port,
                        tag: Tag::Wireguard,
                        stream_settings: None,
                        awg: None,
                        wg: wg_config,
                        h2: None,
                        mtproto_secret: None,
                    },
                );
            }

            #[cfg(feature = "amnezia-wg")]
            if let Some(ref config) = awg_config {
                inbounds.insert(
                    Tag::AmneziaWg,
                    Inbound {
                        port: config.interface.listen_port,
                        tag: Tag::AmneziaWg,
                        stream_settings: None,
                        awg: awg_config,
                        wg: None,
                        h2: None,
                        mtproto_secret: None,
                    },
                );
            }

            #[cfg(feature = "amnezia-wg")]
            if let Some(ref config) = awg_mobile_config {
                inbounds.insert(
                    Tag::AmneziaWgMobile,
                    Inbound {
                        port: config.interface.listen_port,
                        tag: Tag::AmneziaWgMobile,
                        stream_settings: None,
                        awg: awg_mobile_config,
                        wg: None,
                        h2: None,
                        mtproto_secret: None,
                    },
                );
            }

            if let Some(ref config) = mtproto_config {
                inbounds.insert(
                    Tag::Mtproto,
                    Inbound {
                        port: config.port,
                        tag: Tag::Mtproto,
                        stream_settings: None,
                        wg: None,
                        awg: None,
                        h2: None,
                        mtproto_secret: Some(config.secret[0].key.clone()),
                    },
                );
            }

            if let Some(ref config) = h2_config {
                inbounds.insert(
                    Tag::Hysteria2,
                    Inbound {
                        port: config.port,
                        tag: Tag::Hysteria2,
                        stream_settings: None,
                        wg: None,
                        awg: None,
                        h2: h2_config,
                        mtproto_secret: None,
                    },
                );
            }
        };
        Self {
            uuid: settings.uuid,
            env: settings.env,
            hostname: settings.hostname,
            status: Status::Online,
            address: settings.address,
            created_at: now,
            label: settings.label,
            interface: settings.default_interface,
            modified_at: now,
            inbounds,
            cores: settings.cores,
            max_bandwidth_bps: settings.max_bandwidth_bps,
            country: settings.country,
            r#type: settings.r#type,
            cluster: settings.cluster,
        }
    }

    pub fn get_base_tags(&self) -> BTreeMap<String, String> {
        let mut tags = BTreeMap::new();
        tags.insert("env".to_string(), self.env.to_string());
        tags.insert("hostname".to_string(), self.hostname.clone());
        tags.insert("label".to_string(), self.label.clone());
        tags.insert("address".to_string(), self.address.to_string());
        tags.insert("label".to_string(), self.label.clone());
        tags.insert("cores".to_string(), self.cores.to_string());
        tags.insert(
            "max_bandwidth_bps".to_string(),
            self.max_bandwidth_bps.to_string(),
        );
        tags.insert("country".to_string(), self.country.clone());
        tags.insert("type".to_string(), self.r#type.to_string());
        if let Some(cluster) = &self.cluster {
            tags.insert("cluster".to_string(), cluster.clone());
        }
        tags
    }

    pub fn as_node_response(&self) -> NodeResponse {
        let inbounds: Vec<InboundResponse> = self
            .inbounds
            .values()
            .map(|inbound| inbound.as_inbound_response())
            .collect();

        NodeResponse {
            env: self.env.to_string(),
            hostname: self.hostname.clone(),
            interface: self.interface.clone(),
            address: self.address,
            uuid: self.uuid,
            inbounds,
            status: self.status,
            label: self.label.clone(),
            cores: self.cores,
            max_bandwidth_bps: self.max_bandwidth_bps,
            metrics: [].to_vec(),
            country: self.country.clone(),
            r#type: self.r#type,
            cluster: self.cluster.clone(),
        }
    }

    pub fn update_status(&mut self, new_status: Status) -> Result<(), String> {
        self.status = new_status;
        Ok(())
    }

    pub fn inbound(&self, tag: Tag) -> Option<&Inbound> {
        self.inbounds.values().find(|i| i.tag == tag)
    }

    /// Return the cluster domain if the node belongs to a cluster,
    /// otherwise fall back to the node IPv4 address.
    pub fn connection_host(&self) -> String {
        self.cluster
            .clone()
            .unwrap_or_else(|| self.address.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::inbound::Inbound;
    use chrono::Utc;
    use std::collections::HashMap;

    #[test]
    fn test_as_node_response_includes_inbound_settings() {
        let inbound = Inbound {
            tag: Tag::VlessTcpReality,
            port: 443,
            stream_settings: None,
            wg: None,
            awg: None,
            h2: None,
            mtproto_secret: None,
        };

        let mut inbounds = HashMap::new();
        inbounds.insert(Tag::VlessTcpReality, inbound.clone());

        let node = Node {
            uuid: uuid::Uuid::parse_str("ab514c21-aaaa-bbbb-cccc-32f8cb1ada40").unwrap(),
            env: Env::Experimental,
            hostname: "test-node".to_string(),
            address: "192.168.1.100".parse().unwrap(),
            status: Status::Online,
            label: "Test".to_string(),
            interface: "eth0".to_string(),
            created_at: Utc::now(),
            modified_at: Utc::now(),
            inbounds,
            cores: 4,
            max_bandwidth_bps: 1_000_000_000,
            country: "RU".to_string(),
            r#type: Type::Node,
            cluster: Some("test-cluster.example.com".to_string()),
        };

        let response = node.as_node_response();
        assert_eq!(response.inbounds.len(), 1);
        assert_eq!(response.inbounds[0].tag, Tag::VlessTcpReality);
        assert_eq!(response.inbounds[0].port, 443);
        assert_eq!(
            response.cluster,
            Some("test-cluster.example.com".to_string())
        );
    }
}
