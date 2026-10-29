use serde::{de::DeserializeOwned, Deserialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::net::Ipv4Addr;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::memory::{env::Env, node::Type};

#[derive(Clone, Debug, Deserialize, Default)]
pub struct ApiAccessConfig {
    pub endpoint: String,
    pub token: String,
}

fn default_metrics_enabled() -> bool {
    true
}

fn default_metrics_endpoint() -> String {
    "0.0.0.0:3000".to_string()
}

#[derive(Clone, Default, Debug, Deserialize)]
pub struct MetricsTxConfig {
    pub publisher: String,
    pub interval: u64,
    #[serde(default = "default_metrics_enabled")]
    pub enabled: bool,
    #[serde(default = "default_metrics_endpoint")]
    pub endpoint: String,

    /// Токен для /healthcheck на ноде. Пусто — ручка закрыта (404).
    #[serde(default)]
    pub healthcheck_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NodeConfig {
    pub env: Env,
    pub hostname: String,
    pub default_interface: String,
    pub address: Ipv4Addr,
    pub uuid: Uuid,
    pub label: String,
    pub max_bandwidth_bps: i64,
    pub cores: usize,
    pub country: String,
    pub r#type: Type,
    pub cluster: Option<String>,
    /// Extra entry IPs of the node (node_ips[0] == `address`); None =
    /// single-address node.
    pub node_ips: Option<Vec<Ipv4Addr>>,
    /// Optional pool/group name (config.toml `[node] pool`). Clients group
    /// identically-named nodes and may localize the title via
    /// `pool_translation`. None = old node / no pool.
    pub pool: Option<String>,
    /// Per-language translations of `pool` (e.g. en/ru/uk).
    pub pool_translation: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NodeConfigRaw {
    pub env: Env,
    pub hostname: Option<String>,
    pub default_interface: String,
    pub address: Ipv4Addr,
    pub uuid: Uuid,
    pub label: String,
    pub max_bandwidth_bps: i64,
    pub country: String,
    pub r#type: String,
    #[serde(default)]
    pub cluster: Option<String>,
    /// Extra entry IPs: the first must equal `address` (primary first);
    /// all IPs share the same ports/keys/inbounds. Absent = single address.
    #[serde(default)]
    pub node_ips: Option<Vec<Ipv4Addr>>,
    /// Optional pool/group name; a blank value is treated as absent.
    #[serde(default)]
    pub pool: Option<String>,
    /// Optional map lang -> translated pool name; blank entries are dropped
    /// and `ua` is accepted as an alias of `uk`.
    #[serde(default)]
    pub pool_translation: Option<BTreeMap<String, String>>,
}

/// Trims the configured pool name and treats a blank value as absent.
fn normalize_pool(pool: Option<String>) -> Option<String> {
    pool.map(|p| p.trim().to_string()).filter(|p| !p.is_empty())
}

/// Normalizes a `pool_translation` map: trims keys/values, lowercases the
/// language code, aliases `ua` to `uk` and drops blank entries. An empty
/// result becomes `None` so a partially-filled config never wipes stored
/// translations with empty strings.
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

impl NodeConfig {
    pub fn from_raw(raw: NodeConfigRaw) -> Result<NodeConfig> {
        let num_cpus = std::thread::available_parallelism()?.get();
        let hostname = if raw.hostname.is_none() {
            match env::var("HOSTNAME") {
                Ok(hostname) => hostname,
                Err(_) => {
                    return Err(Error::Custom("Validation error: missing hostname (set $HOSTNAME env or specify in config)".into()));
                }
            }
        } else {
            raw.hostname.unwrap()
        };

        Ok(NodeConfig {
            env: raw.env,
            hostname,
            default_interface: raw.default_interface,
            address: raw.address,
            uuid: raw.uuid,
            label: raw.label,
            max_bandwidth_bps: raw.max_bandwidth_bps,
            cores: num_cpus,
            country: raw.country,
            r#type: raw.r#type.parse().unwrap_or(Type::Node),
            cluster: raw.cluster,
            node_ips: raw.node_ips,
            pool: normalize_pool(raw.pool),
            pool_translation: normalize_pool_translation(raw.pool_translation),
        })
    }
}

pub trait Settings: Sized {
    fn read_config<T: DeserializeOwned>(config_file: &str) -> Result<T> {
        let config_str = fs::read_to_string(config_file)?;
        let settings: T = toml::from_str(&config_str)?;
        Ok(settings)
    }

    fn from_file(config_file: &str) -> Self
    where
        for<'de> Self: Deserialize<'de>,
    {
        match Self::read_config(config_file) {
            Ok(settings) => settings,
            Err(e) => panic!("Failed to load settings: {}", e),
        }
    }

    fn validate(&self) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_translation_normalizes_ua_and_blanks() {
        let mut raw = BTreeMap::new();
        raw.insert(" UA ".to_string(), " Фінляндія ".to_string());
        raw.insert("ru".to_string(), "   ".to_string());
        raw.insert("en".to_string(), "Finland".to_string());

        let normalized = normalize_pool_translation(Some(raw)).expect("non-empty map");
        assert_eq!(normalized.get("uk").map(String::as_str), Some("Фінляндія"));
        assert_eq!(normalized.get("en").map(String::as_str), Some("Finland"));
        assert!(!normalized.contains_key("ru"));
        assert!(!normalized.contains_key("ua"));
    }

    #[test]
    fn pool_translation_empty_or_blank_is_none() {
        assert!(normalize_pool_translation(None).is_none());
        assert!(normalize_pool_translation(Some(BTreeMap::new())).is_none());
    }

    #[test]
    fn pool_blank_becomes_none() {
        assert!(normalize_pool(Some("   ".to_string())).is_none());
        assert_eq!(
            normalize_pool(Some(" Finland ".to_string())),
            Some("Finland".to_string())
        );
    }
}
