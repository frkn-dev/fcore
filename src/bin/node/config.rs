use serde::Deserialize;

use fcore::{ApiAccessConfig, MetricsTxConfig, NodeConfigRaw, Result, Settings};

fn default_disabled() -> bool {
    false
}

fn default_log_level() -> String {
    "debug".to_string()
}

#[derive(Clone, Debug, Deserialize)]
pub struct ServiceSettings {
    #[serde(default)]
    pub service: ServiceConfig,
    #[cfg(feature = "xray")]
    #[serde(default)]
    pub xray: XrayConfig,
    #[cfg(feature = "wireguard")]
    #[serde(default)]
    pub wg: WgConfig,
    #[cfg(feature = "amnezia-wg")]
    #[serde(default)]
    pub awg: AmneziaWgConfig,
    #[cfg(feature = "amnezia-wg")]
    #[serde(default)]
    pub awg_mobile: AmneziaWgConfig,
    #[serde(default)]
    pub h2: H2Config,
    #[serde(default)]
    pub mtproto: MtprotoConfig,
    pub node: NodeConfigRaw,
    #[serde(default)]
    pub api: ApiAccessConfig,
    #[serde(default)]
    pub metrics: MetricsTxConfig,
}

impl Settings for ServiceSettings {
    fn validate(&self) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct ServiceConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    pub snapshot_interval: u64,
    pub snapshot_path: String,
    pub updates_endpoint_zmq: String,
}

#[cfg(feature = "xray")]
#[derive(Clone, Debug, Deserialize, Default)]
pub struct XrayConfig {
    #[serde(default = "default_disabled")]
    pub enabled: bool,
    pub path: String,
}

#[derive(Clone, Default, Debug, Deserialize)]
pub struct H2Config {
    #[serde(default = "default_disabled")]
    pub enabled: bool,
    pub path: String,
}

#[cfg(feature = "wireguard")]
#[derive(Clone, Default, Debug, Deserialize)]
pub struct WgConfig {
    #[serde(default = "default_disabled")]
    pub enabled: bool,
    pub path: String,
}

#[cfg(feature = "amnezia-wg")]
#[derive(Clone, Default, Debug, Deserialize)]
pub struct AmneziaWgConfig {
    #[serde(default = "default_disabled")]
    pub enabled: bool,
    pub path: String,
}

#[derive(Clone, Default, Debug, Deserialize)]
pub struct MtprotoConfig {
    #[serde(default = "default_disabled")]
    pub enabled: bool,
    pub path: String,
}
