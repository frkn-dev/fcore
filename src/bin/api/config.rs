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
        Ok(())
    }
}

fn default_base_url() -> String {
    "http://localhost:8080".to_string()
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
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_wg_network")]
    pub wireguard_network: IpAddrMask,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    pub updates_endpoint_zmq: String,
    pub enabled_envs: Vec<Env>,
    pub enabled_tags: Vec<Tag>,
    pub trial_days: i64,
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
}

#[derive(Clone, Default, Debug, Deserialize)]
pub struct MetricsRxConfig {
    pub reciever: String,
    pub max_points: usize,
    pub retention_seconds: i64,
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
