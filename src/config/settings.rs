use serde::{de::DeserializeOwned, Deserialize};
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
