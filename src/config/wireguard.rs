use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

use crate::memory::connection::wireguard::IpAddrMask;
use crate::{error::Error, WgKeys};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireguardServerConfig {
    pub interface: String,
    pub address: String,
    pub port: u16,
    pub private_key: String,
    pub dns: Option<Vec<String>>,
}

impl WireguardServerConfig {
    pub fn from_file(path: &str) -> Result<Self, Error> {
        let contents = std::fs::read_to_string(path)?;

        let mut private_key = None;
        let mut address = None;
        let mut dns = vec![];
        let mut port = None;

        let interface = path
            .split('/')
            .next_back()
            .and_then(|f| f.split('.').next())
            .ok_or_else(|| Error::Custom("no interface name".into()))?
            .to_string();

        for line in contents.lines() {
            let line = line.trim();

            if line.starts_with("PrivateKey") {
                if let Some((_, value)) = line.split_once('=') {
                    private_key = Some(value.trim().to_string());
                } else {
                    return Err(Error::Custom("Invalid PrivateKey line".into()));
                }
            }

            if line.starts_with("Address") {
                address = line.split('=').nth(1).map(|v| v.trim().to_string());
            }

            if line.starts_with("ListenPort") {
                port = line
                    .split('=')
                    .nth(1)
                    .and_then(|v| v.trim().parse::<u16>().ok());
            }

            if line.starts_with("DNS") {
                dns = line
                    .split('=')
                    .nth(1)
                    .unwrap_or("")
                    .split(',')
                    .filter_map(|v| v.trim().parse().ok())
                    .collect();
            }
        }

        Ok(Self {
            interface,
            port: port.ok_or_else(|| Error::Custom("no ListenPort".into()))?,
            private_key: private_key.ok_or_else(|| Error::Custom("no PrivateKey".into()))?,
            address: address.ok_or_else(|| Error::Custom("no Address".into()))?,
            dns: Some(dns),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WireguardSettings {
    pub interface: String,
    pub address: IpAddrMask,
    pub port: u16,
    pub keys: WgKeys,
    pub dns: Vec<Ipv4Addr>,
    /// PersistentKeepalive (seconds) for client configs, set from the node's
    /// config.toml ([wg] keepalive). None = clients get the default 25.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keepalive: Option<u16>,
}

impl std::fmt::Display for WireguardSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}|{}|{}|{}|{}",
            self.interface,
            self.address,
            self.keys.privkey,
            self.port,
            self.dns
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

impl TryFrom<WireguardServerConfig> for WireguardSettings {
    type Error = Error;

    fn try_from(cfg: WireguardServerConfig) -> Result<Self, Error> {
        let keys = WgKeys {
            privkey: cfg.private_key,
        };

        let address = cfg
            .address
            .parse::<IpAddrMask>()
            .map_err(|_| Error::Custom("Invalid WG address".into()))?;

        let dns = cfg
            .dns
            .unwrap_or_default()
            .into_iter()
            .map(|d| d.parse::<Ipv4Addr>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Error::Custom("Invalid DNS".into()))?;

        Ok(Self {
            interface: cfg.interface,
            keys,
            address,
            port: cfg.port,
            dns,
            keepalive: None,
        })
    }
}
