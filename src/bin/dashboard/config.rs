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

#[derive(Clone, Debug, Deserialize)]
pub struct PixelConfig {
    pub endpoint: String,
    pub token: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PaymentConfig {
    pub endpoint: String,
    pub analytics_token: String,
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
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub struct Config {
    pub pixel: PixelConfig,
    pub payment: PaymentConfig,
    pub mrkting: MrktingConfig,

    #[serde(default)]
    pub dashboard: DashboardConfig,
}

impl Config {
    pub fn from_file(path: &str) -> Self {
        let content = std::fs::read_to_string(path).expect("Failed to read config file");
        toml::from_str(&content).expect("Failed to parse config file")
    }
}
