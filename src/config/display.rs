use serde::Deserialize;
use std::collections::HashMap;

/// Optional presentation settings, read on each request so they can be edited
/// without restarting the service. The containing directory is mounted by the deployer.
#[derive(Default, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    pub title: Option<String>,
    pub website: Option<String>,
    pub support: Option<String>,
    pub protocol_names: HashMap<String, String>,
}

impl DisplayConfig {
    pub fn load() -> Self {
        let Some(path) = std::env::var_os("FCORE_PROFILE_CONFIG") else {
            return Self::default();
        };
        match std::fs::read_to_string(path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(config) => config,
                Err(error) => {
                    tracing::warn!("Could not parse profile display settings: {error}");
                    Self::default()
                }
            },
            Err(error) => {
                tracing::warn!("Could not read profile display settings: {error}");
                Self::default()
            }
        }
    }
}

pub fn connection_name(label: &str, protocol: &str, default: &str) -> String {
    let settings = DisplayConfig::load();
    let name = settings.protocol_names.get(protocol).map(String::as_str).unwrap_or(default);
    if name.is_empty() { label.to_string() } else { format!("{label} · {name}") }
}
