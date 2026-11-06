use serde::Deserialize;

use fcore::{ApiAccessConfig, MetricsTxConfig, NodeConfigRaw, Result, Settings};

fn default_disabled() -> bool {
    false
}

fn default_log_level() -> String {
    // Не debug: при опечатке в ключе (например `log_velel`) сюда падает
    // значение по умолчанию, и debug печатает приватные ключи в journalctl.
    // Debug включается только явно.
    "info".to_string()
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
    #[serde(default)]
    pub snapshot: SnapshotConfig,
}

fn default_true() -> bool {
    true
}
fn default_divergence_percent() -> u64 {
    5
}
fn default_divergence_records() -> u64 {
    20
}
fn default_drift_check_sec() -> u64 {
    3600
}

/// Startup validation and the hourly drift check of the snapshot against api.
///
/// The snapshot is only trusted when the counts it carries still match what the
/// api would send for this env + tag: a file that no longer describes reality
/// used to be applied as-is, leaving the node with a subset of the peers.
#[derive(Clone, Debug, Deserialize)]
pub struct SnapshotConfig {
    #[serde(default = "default_true")]
    pub validate_on_start: bool,
    /// Divergence is significant when it exceeds this share of the api count…
    #[serde(default = "default_divergence_percent")]
    pub max_divergence_percent: u64,
    /// …or this many records, whichever is larger.
    #[serde(default = "default_divergence_records")]
    pub max_divergence_records: u64,
    /// Hourly drift check against api; 0 disables it.
    #[serde(default = "default_drift_check_sec")]
    pub drift_check_sec: u64,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            validate_on_start: true,
            max_divergence_percent: default_divergence_percent(),
            max_divergence_records: default_divergence_records(),
            drift_check_sec: default_drift_check_sec(),
        }
    }
}

impl SnapshotConfig {
    /// Is `local` vs `api` a divergence worth rejecting the snapshot for?
    ///
    /// Live creates and deletes run constantly, so an exact match would reject
    /// the file every time somebody connects. Percent OR records: a big node
    /// tolerates 5 %, a small one gets at least 20 records of slack.
    pub fn is_divergent(&self, local: u64, api: u64) -> bool {
        let diff = local.abs_diff(api);
        let percent_limit = (api as u128 * self.max_divergence_percent as u128 / 100) as u64;
        diff > percent_limit.max(self.max_divergence_records)
    }
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
    /// PersistentKeepalive (seconds) injected into client configs. Cannot
    /// live in the interface .conf: wg/awg refuse to start with a keepalive
    /// in [Interface]. None = clients get the default 25.
    #[serde(default)]
    pub keepalive: Option<u16>,
}

#[cfg(feature = "amnezia-wg")]
#[derive(Clone, Default, Debug, Deserialize)]
pub struct AmneziaWgConfig {
    #[serde(default = "default_disabled")]
    pub enabled: bool,
    pub path: String,
    /// PersistentKeepalive (seconds) injected into client configs. Cannot
    /// live in the interface .conf: wg/awg refuse to start with a keepalive
    /// in [Interface]. None = clients get the default 25.
    #[serde(default)]
    pub keepalive: Option<u16>,
}

#[derive(Clone, Default, Debug, Deserialize)]
pub struct MtprotoConfig {
    #[serde(default = "default_disabled")]
    pub enabled: bool,
    pub path: String,
}

#[cfg(test)]
mod snapshot_cfg_tests {
    use super::*;

    fn cfg() -> SnapshotConfig {
        SnapshotConfig::default()
    }

    #[test]
    fn small_drift_is_tolerated() {
        // Live creates and deletes run constantly: an exact match would throw
        // the snapshot away on every restart.
        let c = cfg();
        assert!(!c.is_divergent(1053, 1055));
        assert!(!c.is_divergent(1035, 1055)); // 20 records of slack
    }

    #[test]
    fn big_drift_rejects_the_snapshot() {
        let c = cfg();
        // The production case: 103 peers of 1055.
        assert!(c.is_divergent(103, 1055));
        // 6 % of 1000 is 60 > max(5 %, 20).
        assert!(c.is_divergent(1060, 1000));
    }

    #[test]
    fn percent_applies_to_big_nodes_records_to_small_ones() {
        let c = cfg();
        // 5 % of 10000 = 500 records of slack.
        assert!(!c.is_divergent(9600, 10000));
        assert!(c.is_divergent(9400, 10000));
        // Small node: 20 records win over 5 % of 20 (=1).
        assert!(!c.is_divergent(35, 20));
        assert!(c.is_divergent(45, 20));
    }
}
