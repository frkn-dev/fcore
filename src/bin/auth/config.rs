use serde::Deserialize;
use std::net::Ipv4Addr;

use fcore::{ApiAccessConfig, MetricsTxConfig, NodeConfigRaw, Result, Settings};

#[derive(Clone, Debug, Deserialize)]
pub struct ServiceSettings {
    pub service: ServiceConfig,
    pub node: NodeConfigRaw,
    pub api: ApiAccessConfig,
    pub metrics: MetricsTxConfig,
}

impl Settings for ServiceSettings {
    fn validate(&self) -> Result<()> {
        Ok(())
    }
}

fn default_listen_address() -> Ipv4Addr {
    "127.0.0.1".parse().unwrap()
}

fn default_listen_port() -> u16 {
    3000
}

fn default_cors_origin() -> String {
    "http://localhost:8080".to_string()
}

#[derive(Clone, Debug, Deserialize)]
pub struct ServiceConfig {
    pub log_level: String,
    #[serde(default = "default_listen_address")]
    pub listen: Ipv4Addr,
    #[serde(default = "default_listen_port")]
    pub port: u16,
    pub snapshot_interval: u64,
    pub snapshot_path: String,
    #[serde(default = "default_cors_origin")]
    pub origin: String,
    pub updates_endpoint_zmq: String,
}
