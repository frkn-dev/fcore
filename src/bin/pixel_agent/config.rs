use serde::Deserialize;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,

    pub log_path: PathBuf,

    #[serde(default = "default_offset_path")]
    pub offset_path: PathBuf,

    pub geoip_db: PathBuf,

    #[serde(default = "default_snapshot_path")]
    pub snapshot_path: PathBuf,

    #[serde(default = "default_admin_listen")]
    pub admin_listen: String,

    #[serde(default = "default_admin_port")]
    pub admin_port: u16,

    #[serde(default = "default_poll_interval_sec")]
    pub poll_interval_sec: u64,

    #[serde(default = "default_flush_interval_sec")]
    pub flush_interval_sec: u64,

    #[serde(default = "default_snapshot_interval_sec")]
    pub snapshot_interval_sec: u64,

    #[serde(default = "default_bucket_minutes")]
    pub bucket_minutes: u64,

    #[serde(default = "default_retention_hours")]
    pub retention_hours: u64,

    #[serde(default = "default_max_points")]
    pub max_points: usize,

    #[serde(default = "default_retention_seconds")]
    pub retention_seconds: i64,
}

impl AgentConfig {
    pub fn from_file(path: &str) -> Self {
        let content = std::fs::read_to_string(path).expect("Failed to read config file");
        toml::from_str(&content).expect("Failed to parse config file")
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_offset_path() -> PathBuf {
    PathBuf::from("/var/lib/pixel-agent/offset.dat")
}

fn default_snapshot_path() -> PathBuf {
    PathBuf::from("/var/lib/pixel-agent/metrics.snapshot")
}

fn default_admin_listen() -> String {
    "0.0.0.0".to_string()
}

fn default_admin_port() -> u16 {
    9102
}

fn default_poll_interval_sec() -> u64 {
    30
}

fn default_flush_interval_sec() -> u64 {
    300
}

fn default_snapshot_interval_sec() -> u64 {
    300
}

fn default_bucket_minutes() -> u64 {
    5
}

fn default_retention_hours() -> u64 {
    168
}

fn default_max_points() -> usize {
    10000
}

fn default_retention_seconds() -> i64 {
    7 * 24 * 60 * 60
}
