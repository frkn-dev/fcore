use serde::Deserialize;

fn default_listen() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    9103
}

fn default_refresh_sec() -> u64 {
    30
}

fn default_session_ttl_hours() -> i64 {
    168
}

#[derive(Clone, Debug, Deserialize)]
pub struct PostgresConfig {
    pub host: String,
    pub port: u16,
    pub db: String,
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PixelConfig {
    pub endpoint: String,
    pub token: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PaymentConfig {
    pub endpoint: String,
    pub analytics_token: String,
    /// Token for administrative operations in payment-gateway, such as
    /// creating or deleting partner promocodes. If omitted, promocode
    /// sync is skipped.
    pub admin_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MrktingConfig {
    pub endpoint: String,
    pub token: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[allow(dead_code)]
pub struct DashboardConfig {
    #[serde(default = "default_listen")]
    pub listen: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_refresh_sec")]
    pub refresh_sec: u64,

    /// Optional bearer token for the main dashboard API endpoints
    /// (/api/overview, /api/sales, /api/pixel). If set, requests must include
    /// `Authorization: Bearer <token>`.
    pub api_token: Option<String>,

    /// Optional bearer token for administrative dashboard endpoints, such as
    /// attaching existing payment-gateway promocodes to partners.
    pub admin_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PartnerConfig {
    #[serde(default = "default_session_ttl_hours")]
    pub session_ttl_hours: i64,
}

impl Default for PartnerConfig {
    fn default() -> Self {
        Self {
            session_ttl_hours: default_session_ttl_hours(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub struct Config {
    pub pg: PostgresConfig,
    pub pixel: PixelConfig,
    pub payment: PaymentConfig,
    pub mrkting: MrktingConfig,

    #[serde(default)]
    pub dashboard: DashboardConfig,

    #[serde(default)]
    pub partner: PartnerConfig,
}

impl Config {
    pub fn from_file(path: &str) -> Self {
        let content = std::fs::read_to_string(path).expect("Failed to read config file");
        toml::from_str(&content).expect("Failed to parse config file")
    }
}
