use serde::Deserialize;
use std::collections::HashMap;
use std::net::Ipv4Addr;

use fcore::{Env, IpAddrMask, Result, Settings, Tag};

#[derive(Clone, Debug, Deserialize)]
pub struct ServiceSettings {
    pub service: ServiceConfig,
    pub pg: PostgresConfig,
    pub metrics: MetricsRxConfig,
    pub tasks: TasksConfig,
    pub subscription_audit: SubscriptionAuditConfig,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MrktingConfig {
    pub endpoint: String,
    pub token: String,
}

fn default_apple_allowed_products() -> Vec<String> {
    vec![
        "frkn_premium_1_month".to_string(),
        "frkn_premium_3_month".to_string(),
        "frkn_premium_6_month".to_string(),
        "frkn_premium_12_month".to_string(),
    ]
}

/// App Store IAP (App Store Server API) configuration.
/// Without this section the service starts normally, but `POST /v1/subscriptions` answers 503.
#[derive(Clone, Debug, Deserialize)]
pub struct AppleConfig {
    pub key_id: String,
    pub issuer_id: String,
    /// Path to the App Store Connect .p8 private key (PKCS#8 PEM).
    pub private_key_path: String,
    pub bundle_id: String,
    /// "Sandbox" or "Production".
    pub environment: String,
    /// Required when environment = "Production".
    pub app_apple_id: Option<i64>,
    /// Path to the Apple Root CA certificate (DER), e.g. AppleRootCA-G3.cer.
    pub root_ca_path: String,
    /// Product IDs allowed for binding. An empty list disables the restriction.
    #[serde(default = "default_apple_allowed_products")]
    pub allowed_products: Vec<String>,
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
    #[serde(default = "default_cors_origins")]
    pub cors_origins: Vec<String>,
    #[serde(default = "default_wg_network")]
    pub wireguard_network: IpAddrMask,
    #[serde(default = "default_wg_network")]
    pub amnezia_wireguard_network: IpAddrMask,
    /// Dedicated pool for AmneziaWgMobile (e.g. 10.77.0.0/16) so mobile
    /// clients avoid the CGNAT range. Without it AmneziaWgMobile
    /// connections cannot be created.
    #[serde(default)]
    pub amnezia_wireguard_mobile_network: Option<IpAddrMask>,
    #[serde(default)]
    pub enabled_conns: Option<HashMap<Env, Vec<Tag>>>,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    pub updates_endpoint_zmq: String,
    pub subscription_title: String,
    pub support_contact: String,
    pub base_url: String,
    #[serde(default)]
    pub admin_enabled: bool,
    pub admin_token: Option<String>,
    #[serde(default)]
    pub agw_private_key_path: Option<String>,
    #[serde(default)]
    pub mrkting: Option<MrktingConfig>,
    #[serde(default)]
    pub apple: Option<AppleConfig>,
    /// Labels shown on service cards in the client (`/v1/services`).
    #[serde(default)]
    pub gateway_price_label: Option<String>,
    #[serde(default)]
    pub gateway_speed_label: Option<String>,
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

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct SubscriptionAuditConfig {
    pub enabled: bool,
    pub directory: String,
    pub file: String,
    pub rotation: String,
    pub level: String,
}

impl Default for SubscriptionAuditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            directory: "logs".to_string(),
            file: "subscription_audit.log".to_string(),
            rotation: "daily".to_string(),
            level: "info".to_string(),
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
