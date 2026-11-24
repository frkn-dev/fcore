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
    /// Registered and serving, but its inbounds do not match the rest of its
    /// pool. Kept in every serving path on purpose: a configuration mismatch
    /// must not cut clients off from a node that still works.
    Degraded,
    Offline,
}

impl Status {
    /// True for every node that may serve clients (`Online` and `Degraded`).
    pub fn is_serving(self) -> bool {
        matches!(self, Status::Online | Status::Degraded)
    }
}

impl PartialEq for Status {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Status::Online, Status::Online)
                | (Status::Degraded, Status::Degraded)
                | (Status::Offline, Status::Offline)
        )
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Online => write!(f, "Online"),
            Status::Degraded => write!(f, "Degraded"),
            Status::Offline => write!(f, "Offline"),
        }
    }
}

impl FromStr for Status {
    type Err = ();

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "Online" => Ok(Status::Online),
            "Degraded" => Ok(Status::Degraded),
            "Offline" => Ok(Status::Offline),
            _ => Ok(Status::Offline),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Copy, ToSql, FromSql, PartialEq)]
#[postgres(name = "node_type")]
pub enum Type {
    /// Legacy label. Old fnode builds register as `common`; new ones send
    /// `core`. Both mean the same thing: a machine that serves client traffic.
    #[postgres(name = "common")]
    Node,

    #[postgres(name = "core")]
    Core,

    /// A user's own machine (self-hosted subscription).
    #[postgres(name = "selfhosted")]
    SelfHosted,

    #[postgres(name = "premium")]
    PremiumNode,

    #[postgres(name = "service")]
    Service,

    #[postgres(name = "agent")]
    Agent,
}

impl Type {
    /// True for both the legacy `common` label and the new `core` one.
    pub fn is_core(&self) -> bool {
        matches!(self, Type::Node | Type::Core)
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Node | Type::Core => write!(f, "core"),
            Type::SelfHosted => write!(f, "selfhosted"),
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
            // `common` is what old fnode builds send; normalise it to `core`
            // so freshly registered machines carry the new label.
            "core" | "common" | "node" => Ok(Type::Core),
            "premium" | "premiumnode" | "premium_node" => Ok(Type::PremiumNode),
            "service" => Ok(Type::Service),
            "agent" => Ok(Type::Agent),
            "selfhosted" | "self_hosted" | "personal" => Ok(Type::SelfHosted),
            _ => Err(()),
        }
    }
}

/// One entry point of a node — the single endpoint shape used by the node
/// config (`[[node.endpoints]]`), the node→api announcement and the api's
/// client payload.
///
/// Every field except `ip` is optional on the wire:
/// `port` falls back to the inbound (protocol) port, `weight` to
/// [`DEFAULT_NODE_WEIGHT`], `label` is for logs only. Old payloads without
/// `endpoints` keep working: the api derives the list from the legacy
/// `node_ips` + `node_weights` pair, which this struct mirrors.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Endpoint {
    pub ip: Ipv4Addr,
    /// Port this address listens on; `None` = the inbound's own port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Share of the traffic; `100` (the default) for a regular entry.
    #[serde(default = "default_endpoint_weight")]
    pub weight: i32,
    /// Free-form name for logs/diagnostics; never parsed by a client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

fn default_endpoint_weight() -> i32 {
    DEFAULT_NODE_WEIGHT
}

impl Endpoint {
    /// Wire-shaped endpoint of the primary address, all defaults.
    pub fn from_ip(ip: Ipv4Addr) -> Self {
        Self {
            ip,
            port: None,
            weight: DEFAULT_NODE_WEIGHT,
            label: None,
        }
    }

    /// Same endpoint with `port` filled in from the inbound when unset — the
    /// form the api publishes, so a client never has to know protocol ports.
    pub fn with_default_port(&self, default_port: u16) -> Self {
        Self {
            ip: self.ip,
            port: Some(self.port.unwrap_or(default_port)),
            weight: if self.weight > 0 {
                self.weight
            } else {
                DEFAULT_NODE_WEIGHT
            },
            label: self.label.clone(),
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
    /// Entry points in `[[node.endpoints]]` order, port resolved per protocol
    /// where known. Absent for nodes that declare none (old node builds).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<Endpoint>,
    /// `true` when the node still declares the legacy `node_ips`/`node_weights`
    /// keys. Counted across the fleet to know when the legacy path can be
    /// removed (`#deprecated` in `config/settings.rs`).
    #[serde(default)]
    pub endpoints_legacy: bool,
    /// Extra entry IPs of the node: node_ips[0] is the primary address
    /// (== `address`); absent = single-address node (current behavior).
    /// Skipped in serialization when absent so old consumers see the
    /// byte-for-byte old shape.
    ///
    /// TODO(remove-legacy-endpoints): legacy mirror of `endpoints`, kept until
    /// every node reports `endpoints_legacy: false` and no old client reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_ips: Option<Vec<Ipv4Addr>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Per-entry weights (config.toml `[node] node_weights`), keyed by an
    /// address from `node_ips`. Absent when every address uses the default (100).
    ///
    /// TODO(remove-legacy-endpoints): legacy mirror of `endpoints`, kept until
    /// every node reports `endpoints_legacy: false` and no old client reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_weights: Option<BTreeMap<String, i32>>,
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
    /// Extra entry IPs of the node: node_ips[0] is the primary address
    /// (== `address`); absent = single-address node (current behavior).
    /// Participates in the derived PartialEq so a changed IP set registers
    /// as a node update in storage (see NodeStorageOperations::add).
    ///
    /// TODO(remove-legacy-endpoints): legacy mirror of `endpoints`, kept until
    /// every node reports `endpoints_legacy: false` and no old client reads it.
    #[serde(default)]
    pub node_ips: Option<Vec<Ipv4Addr>>,
    /// Entry points in `[[node.endpoints]]` order — the single source of truth
    /// for addresses, ports and weights. Empty for nodes whose registration
    /// came from an old build (or from a database written before the column
    /// existed); `node_ips`/`node_weights` are then the only source and are
    /// mirrored here on the next registration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<Endpoint>,
    /// `true` when the node still declares the legacy `node_ips`/`node_weights`
    /// keys in its config. Migration progress counter: the legacy code paths
    /// below stay until no node reports `true`.
    #[serde(default)]
    pub endpoints_legacy: bool,
    /// Optional pool/group name of the node (config.toml `[node] pool`, e.g.
    /// "Finland"). Clients group identically-named nodes and may show a
    /// localized title (`pool_translation`) instead of `label`. Absent for
    /// old nodes, so old clients keep the previous behavior.
    #[serde(default)]
    pub pool: Option<String>,
    /// Per-language translations of `pool` (e.g. en/ru/uk). A missing entry
    /// falls back to `pool`; an absent map means there is no translation.
    #[serde(default)]
    pub pool_translation: Option<BTreeMap<String, String>>,
    /// Country of the whole pool (`[node] pool_country_code`): the flag of the
    /// card, not of this machine. A pool may span countries; when it declares
    /// none the card falls back to the node's own `country`.
    #[serde(default)]
    pub pool_country_code: Option<String>,
    /// Human name of `pool_country_code` (`[node] pool_country_name`); absent
    /// means the client translates the code itself.
    #[serde(default)]
    pub pool_country_name: Option<String>,
    /// Per-entry weights (config.toml `[node] node_weights`), keyed by an
    /// address from `node_ips`. An unlisted address weighs `DEFAULT_NODE_WEIGHT`;
    /// neighbour addresses may share the same weight. Part of the derived
    /// PartialEq, so a weight change in the config registers as a node update.
    #[serde(default)]
    pub node_weights: BTreeMap<String, i32>,
    /// Base URL of the node's own speed-test endpoint
    /// (`https://<hoster>-<cc>-<city>-bench-<nn>.bench.beacon.frkn.org:19443`).
    /// Declared by the node (`[node] bench_url`) and persisted as
    /// `nodes.bench_url`; a node that does not declare one keeps the stored
    /// value on re-registration, and the api never derives it from the
    /// hostname. Exposed as `speedtest_url` in the public `/status`.
    #[serde(default)]
    pub bench_url: Option<String>,
}

/// Default weight of an entry address: a regular node. Lower = smaller share of
/// the traffic.
pub const DEFAULT_NODE_WEIGHT: i32 = 100;

#[allow(deprecated)]
impl Node {
    /// Entry addresses in `node_ips` order, else `address` alone.
    pub fn entry_ips(&self) -> Vec<String> {
        self.node_ips
            .as_ref()
            .map(|ips| ips.iter().map(|ip| ip.to_string()).collect::<Vec<_>>())
            .filter(|ips: &Vec<String>| !ips.is_empty())
            .unwrap_or_else(|| vec![self.address.to_string()])
    }

    /// Endpoints exactly as the node declared them, or derived from the legacy
    /// `node_ips`/`node_weights` pair — never empty.
    ///
    /// A node registered by an old build (or a row written before the
    /// `nodes.endpoints` column existed) has no list, so the api would
    /// otherwise publish `endpoints: []` next to a populated `node_ips`. This
    /// keeps ONE shape on every level regardless of how the node declared
    /// itself; `port: None` still means "the inbound's own port".
    pub fn endpoint_list(&self) -> Vec<Endpoint> {
        if !self.endpoints.is_empty() {
            return self.endpoints.clone();
        }
        let ips: Vec<Ipv4Addr> = match &self.node_ips {
            Some(ips) if !ips.is_empty() => ips.clone(),
            _ => vec![self.address],
        };
        ips.into_iter()
            .map(|ip| Endpoint {
                ip,
                port: None,
                weight: self.weight_for(&ip.to_string()),
                label: None,
            })
            .collect()
    }

    /// Entry points with the port resolved for a protocol's inbound — the form
    /// the api publishes, so a client never has to know protocol ports.
    pub fn endpoints_with_port(&self, default_port: u16) -> Vec<Endpoint> {
        self.endpoint_list()
            .iter()
            .map(|endpoint| endpoint.with_default_port(default_port))
            .collect()
    }

    /// Weight of one address: explicit, else `DEFAULT_NODE_WEIGHT`.
    ///
    /// A non-positive weight counts as "not set": a node running an old build
    /// sends 0 instead of omitting the field, and a zero share of traffic is a
    /// worse default than the regular one.
    pub fn weight_for(&self, ip: &str) -> i32 {
        match self.node_weights.get(ip) {
            Some(w) if *w > 0 => *w,
            _ => DEFAULT_NODE_WEIGHT,
        }
    }

    /// Weights of every entry address — what the client receives (`node_weights`).
    pub fn weights_by_ip(&self) -> BTreeMap<String, i32> {
        self.entry_ips()
            .into_iter()
            .map(|ip| {
                let weight = self.weight_for(&ip);
                (ip, weight)
            })
            .collect()
    }

    /// Highest address weight — the value of the legacy `weight` field.
    pub fn max_weight(&self) -> i32 {
        self.weights_by_ip()
            .values()
            .copied()
            .max()
            .unwrap_or(DEFAULT_NODE_WEIGHT)
    }

    pub fn new(
        settings: NodeConfig,
        #[cfg(feature = "xray")] xray_config: Option<XraySettings>,
        #[cfg(feature = "wireguard")] wg_config: Option<WireguardSettings>,
        #[cfg(feature = "amnezia-wg")] awg_config: Option<AmneziaWgSettings>,
        #[cfg(feature = "amnezia-wg")] awg_mobile_config: Option<AmneziaWgSettings>,
        #[cfg(feature = "amnezia-wg")] awg_peers_config: Option<AmneziaWgSettings>,
        #[cfg(feature = "wireguard")] wg_peers_config: Option<WireguardSettings>,
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

            // Peers plane. Same shape as the legacy inbounds, different tag:
            // the api serves peers from these and never creates connections
            // for them. A node that cannot bring the interface up does not
            // announce the tag, so /v1/locations never shows a dead card.
            #[cfg(feature = "amnezia-wg")]
            if let Some(ref config) = awg_peers_config {
                inbounds.insert(
                    Tag::AmneziaPeer,
                    Inbound {
                        port: config.interface.listen_port,
                        tag: Tag::AmneziaPeer,
                        stream_settings: None,
                        awg: awg_peers_config,
                        wg: None,
                        h2: None,
                        mtproto_secret: None,
                    },
                );
            }

            #[cfg(feature = "wireguard")]
            if let Some(ref config) = wg_peers_config {
                inbounds.insert(
                    Tag::WireguardPeer,
                    Inbound {
                        port: config.port,
                        tag: Tag::WireguardPeer,
                        stream_settings: None,
                        awg: None,
                        wg: wg_peers_config,
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
            node_ips: settings.node_ips,
            endpoints: settings.endpoints.unwrap_or_default(),
            endpoints_legacy: settings.endpoints_legacy,
            pool: settings.pool,
            pool_translation: settings.pool_translation,
            pool_country_code: settings.pool_country_code,
            pool_country_name: settings.pool_country_name,
            node_weights: settings.node_weights.unwrap_or_default(),
            bench_url: settings.bench_url,
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
            // Node view without a protocol: `port: null` means "the inbound's
            // port", which is what the per-protocol client payloads resolve.
            endpoints: self.endpoint_list(),
            endpoints_legacy: self.endpoints_legacy,
            node_ips: self.node_ips.clone(),
            pool: self.pool.clone(),
            node_weights: if self.node_weights.is_empty() {
                None
            } else {
                Some(self.node_weights.clone())
            },
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
    /// An empty cluster string counts as "no cluster".
    pub fn connection_host(&self) -> String {
        match &self.cluster {
            Some(cluster) if !cluster.is_empty() => cluster.clone(),
            _ => self.address.to_string(),
        }
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
            node_ips: None,
            endpoints: Vec::new(),
            endpoints_legacy: false,
            pool: None,
            pool_translation: None,
            pool_country_code: None,
            pool_country_name: None,
            node_weights: BTreeMap::new(),
            bench_url: None,
        };

        let response = node.as_node_response();
        assert_eq!(response.inbounds.len(), 1);
        // The node view carries the ONE endpoint shape too: here it is derived
        // from `address` because the node declares no list.
        assert_eq!(response.endpoints.len(), 1);
        assert_eq!(response.endpoints[0].ip, "192.168.1.100".parse::<Ipv4Addr>().unwrap());
        assert!(!response.endpoints_legacy);
        assert_eq!(response.inbounds[0].tag, Tag::VlessTcpReality);
        assert_eq!(response.inbounds[0].port, 443);
        assert_eq!(
            response.cluster,
            Some("test-cluster.example.com".to_string())
        );
    }

    fn node_with_endpoints(endpoints: Vec<Endpoint>) -> Node {
        Node {
            uuid: uuid::Uuid::nil(),
            env: Env::Experimental,
            hostname: "node".to_string(),
            address: "192.0.2.1".parse().unwrap(),
            status: Status::Online,
            label: "Node".to_string(),
            interface: "eth0".to_string(),
            created_at: Utc::now(),
            modified_at: Utc::now(),
            inbounds: HashMap::new(),
            cores: 1,
            max_bandwidth_bps: 1,
            country: "NL".to_string(),
            r#type: Type::Node,
            cluster: None,
            node_ips: None,
            endpoints,
            endpoints_legacy: false,
            pool: None,
            pool_translation: None,
            pool_country_code: None,
            pool_country_name: None,
            node_weights: BTreeMap::new(),
            bench_url: None,
        }
    }

    /// A declared port wins over the inbound's; an entry without one takes the
    /// inbound port, so a client never sees a portless endpoint.
    #[test]
    fn endpoints_with_port_resolves_the_inbound_fallback() {
        let node = node_with_endpoints(vec![
            Endpoint {
                ip: "192.0.2.1".parse().unwrap(),
                port: Some(8443),
                weight: 200,
                label: None,
            },
            Endpoint {
                ip: "192.0.2.2".parse().unwrap(),
                port: None,
                weight: 50,
                label: None,
            },
        ]);
        let endpoints = node.endpoints_with_port(51820);
        assert_eq!(endpoints[0].port, Some(8443));
        assert_eq!(endpoints[0].weight, 200);
        assert_eq!(endpoints[1].port, Some(51820));
        assert_eq!(endpoints[1].weight, 50);
    }

    /// A node with no declared list (old build, or a row written before the
    /// column existed) expands the legacy pair into the same shape.
    #[test]
    fn legacy_node_expands_into_the_same_endpoint_shape() {
        let mut node = node_with_endpoints(Vec::new());
        node.node_ips = Some(vec![
            "192.0.2.1".parse().unwrap(),
            "192.0.2.2".parse().unwrap(),
        ]);
        node.node_weights = BTreeMap::from([("192.0.2.2".to_string(), 25)]);
        node.endpoints_legacy = true;

        let endpoints = node.endpoints_with_port(443);
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0].ip, "192.0.2.1".parse::<Ipv4Addr>().unwrap());
        assert_eq!(endpoints[0].port, Some(443));
        assert_eq!(endpoints[0].weight, DEFAULT_NODE_WEIGHT);
        assert_eq!(endpoints[1].weight, 25);
    }
}
