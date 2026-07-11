use base64::Engine;
use serde::Deserialize;
use std::net::Ipv4Addr;

use fcore::{Env, Result, Settings, Tag};

#[derive(Clone, Debug, Deserialize)]
pub struct ServiceSettings {
    pub service: ServiceConfig,
    pub pg: PostgresConfig,
    pub api: ApiConfig,
    pub smtp: SmtpConfig,
    pub email_encryption: EmailEncryptionConfig,
    pub trial: TrialConfig,
}

impl Settings for ServiceSettings {
    fn validate(&self) -> Result<()> {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&self.email_encryption.key)
            .map_err(|e| {
                fcore::Error::Custom(format!(
                    "email_encryption.key is not valid base64: {e}"
                ))
            })?;
        if decoded.len() != 32 {
            return Err(fcore::Error::Custom(format!(
                "email_encryption.key must decode to 32 bytes, got {} bytes",
                decoded.len()
            )));
        }
        Ok(())
    }
}

fn default_listen_address() -> Ipv4Addr {
    "127.0.0.1".parse().unwrap()
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_cors_origins() -> Vec<String> {
    vec!["http://localhost:8080".to_string()]
}

#[derive(Clone, Debug, Deserialize)]
pub struct ServiceConfig {
    #[serde(default = "default_listen_address")]
    pub listen: Ipv4Addr,
    pub port: u16,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_cors_origins")]
    pub cors_origins: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct PostgresConfig {
    pub host: String,
    pub port: u16,
    pub db: String,
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ApiConfig {
    pub endpoint: String,
    pub token: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SmtpConfig {
    pub server: String,
    pub username: String,
    pub password: String,
    pub port: u16,
    pub from: String,
    pub title: String,
    pub company_name: String,
    pub support: String,
    #[serde(default = "default_company_website")]
    pub company_website: String,
}

fn default_company_website() -> String {
    "http://localhost:8080".to_string()
}

#[derive(Clone, Debug, Deserialize)]
pub struct EmailEncryptionConfig {
    /// Base64-encoded 32-byte AES-256-GCM key.
    pub key: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TrialConfig {
    pub days: i64,
    pub limit_bytes: i64,
    pub enabled_envs: Vec<Env>,
    pub enabled_tags: Vec<Tag>,
}
