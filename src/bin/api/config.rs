use serde::Deserialize;
use std::net::Ipv4Addr;

use fcore::{Env, IpAddrMask, Result, Settings, Tag};

#[derive(Clone, Debug, Deserialize)]
pub struct ServiceSettings {
    pub service: ServiceConfig,
    pub pg: PostgresConfig,
    pub metrics: MetricsRxConfig,
    pub tasks: TasksConfig,
    pub smtp: SmtpConfig,
}

impl Settings for ServiceSettings {
    fn validate(&self) -> Result<()> {
        let key_path = self
            .service
            .agw_private_key_path
            .as_ref()
            .map(|p| p.trim())
            .unwrap_or_default();
        if key_path.is_empty() {
            return Err(fcore::Error::Custom(
                "service.agw_private_key_path is required".to_string(),
            ));
        }
        Ok(())
    }
}

fn default_cors_origins() -> Vec<String> {
    vec!["http://localhost:8080".to_string()]
}

fn default_wg_network() -> IpAddrMask {
    "10.0.0.0/8".parse().unwrap()
}

fn default_listen_address() -> Ipv4Addr {
    "127.0.0.1".parse().unwrap()
}

fn default_log_level() -> String {
    "debug".to_string()
}

#[derive(Clone, Debug, Deserialize)]
pub struct ServiceConfig {
    #[serde(default = "default_listen_address")]
    pub listen: Ipv4Addr,
    pub port: u16,
    pub token: String,
    pub key_sign_token: Vec<u8>,
    pub bonus_days: i64,
    pub system_refer_codes: Vec<String>,
    #[serde(default = "default_cors_origins")]
    pub cors_origins: Vec<String>,
    #[serde(default = "default_wg_network")]
    pub wireguard_network: IpAddrMask,
    #[serde(default = "default_wg_network")]
    pub amnezia_wireguard_network: IpAddrMask,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    pub updates_endpoint_zmq: String,
    pub enabled_envs: Vec<Env>,
    pub enabled_tags: Vec<Tag>,
    pub trial_limit_days: i64,
    pub trial_limit_bytes: i64,
    pub subscription_title: String,
    pub support_contact: String,
    pub base_url: String,
    #[serde(default)]
    pub admin_enabled: bool,
    pub admin_token: Option<String>,
    #[serde(default)]
    pub agw_private_key_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct PostgresConfig {
    pub host: String,
    pub port: u16,
    pub db: String,
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct TasksConfig {
    pub db_sync_interval_sec: u64,
    pub subscription_restore_interval: u64,
    pub subscription_expire_interval: u64,
    pub connection_expire_interval: u64,
    pub monitor_nodes_interval: u64,
    pub heartbeat_node_offline_threshold_sec: u64,
    #[serde(default = "default_traffic_persist_interval_sec")]
    pub traffic_persist_interval_sec: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MetricsLogConfig {
    pub enabled: bool,
    pub directory: String,
    pub file: String,
    pub rotation: String,
    pub level: String,
}

impl Default for MetricsLogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            directory: "logs".to_string(),
            file: "metrics.log".to_string(),
            rotation: "daily".to_string(),
            level: "debug".to_string(),
        }
    }
}

#[derive(Clone, Default, Debug, Deserialize)]
pub struct MetricsRxConfig {
    pub reciever: String,
    pub max_points: usize,
    pub retention_seconds: i64,
    #[serde(default)]
    pub log: MetricsLogConfig,
    pub snapshot_path: String,
}

fn default_traffic_persist_interval_sec() -> u64 {
    3600
}

fn default_company_website() -> String {
    "http://localhost:8080".to_string()
}
#[derive(Clone, Debug, Deserialize, Default)]
pub struct SmtpConfig {
    pub server: String,
    pub username: String,
    pub password: String,
    pub port: u16,
    pub from: String,
    pub title: String,
    pub company_name: String,
    pub support: String,
    pub email_file: String,
    pub email_sign_token: Vec<u8>,
    #[serde(default = "default_company_website")]
    pub company_website: String,
}
