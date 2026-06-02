use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

use crate::{error::Error, WgKeys};

use crate::memory::connection::wireguard::IpAddrMask;

// =====================
// Obfuscation params
// =====================

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AwgObfuscationParams {
    pub jc: u32,
    pub jmin: u32,
    pub jmax: u32,

    pub s1: u32,
    pub s2: u32,

    pub h1: u32,
    pub h2: u32,
    pub h3: u32,
    pub h4: u32,
}

// =====================
// Interface config
// =====================

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AwgInterfaceConfig {
    pub interface: String,
    pub address: IpAddrMask,
    pub listen_port: u16,
    pub private_key: WgKeys,
    pub dns: Vec<Ipv4Addr>,
}

// =====================
// Full settings
// =====================

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AmneziaWgSettings {
    pub interface: AwgInterfaceConfig,
    pub obfuscation: Option<AwgObfuscationParams>,
}

// =====================
// Raw file config
// =====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmneziaWgServerConfig {
    pub interface: String,
    pub address: String,
    pub port: u16,
    pub private_key: String,
    pub dns: Option<Vec<Ipv4Addr>>,

    pub obfuscation: Option<AwgObfuscationParams>,
}

// =====================
// Parse from file
// =====================

impl AmneziaWgServerConfig {
    pub fn from_file(path: &str) -> Result<Self, Error> {
        let contents = std::fs::read_to_string(path)?;

        let interface = path
            .split('/')
            .next_back()
            .and_then(|f| f.split('.').next())
            .ok_or_else(|| Error::Custom("no interface name".into()))?
            .to_string();

        let mut private_key = None;
        let mut address = None;
        let mut dns: Vec<Ipv4Addr> = vec![];
        let mut port = None;

        let mut jc: Option<u32> = None;
        let mut jmin: Option<u32> = None;
        let mut jmax: Option<u32> = None;
        let mut s1: Option<u32> = None;
        let mut s2: Option<u32> = None;
        let mut h1: Option<u32> = None;
        let mut h2: Option<u32> = None;
        let mut h3: Option<u32> = None;
        let mut h4: Option<u32> = None;

        for line in contents.lines() {
            let line = line.trim();

            let Some((key, value)) = line.split_once('=') else {
                continue;
            };

            let value = value.trim();

            match key.trim() {
                "PrivateKey" => {
                    private_key = Some(value.to_string());
                }

                "Address" => {
                    address = Some(value.to_string());
                }

                "ListenPort" => {
                    port = value.parse::<u16>().ok();
                }

                "DNS" => {
                    dns = value
                        .split(',')
                        .filter_map(|v| v.trim().parse::<Ipv4Addr>().ok())
                        .collect();
                }

                // ===== AWG obfuscation =====
                "Jc" => jc = value.parse().ok(),
                "Jmin" => jmin = value.parse().ok(),
                "Jmax" => jmax = value.parse().ok(),
                "S1" => s1 = value.parse().ok(),
                "S2" => s2 = value.parse().ok(),
                "H1" => h1 = value.parse().ok(),
                "H2" => h2 = value.parse().ok(),
                "H3" => h3 = value.parse().ok(),
                "H4" => h4 = value.parse().ok(),

                _ => {}
            }
        }

        let obfuscation = match jc {
            Some(jc_val) => Some(AwgObfuscationParams {
                jc: jc_val,
                jmin: jmin.unwrap_or(0),
                jmax: jmax.unwrap_or(0),
                s1: s1.unwrap_or(0),
                s2: s2.unwrap_or(0),
                h1: h1.unwrap_or(0),
                h2: h2.unwrap_or(0),
                h3: h3.unwrap_or(0),
                h4: h4.unwrap_or(0),
            }),
            None => None,
        };

        Ok(Self {
            interface,
            port: port.ok_or_else(|| Error::Custom("no ListenPort".into()))?,
            private_key: private_key.ok_or_else(|| Error::Custom("no PrivateKey".into()))?,
            address: address.ok_or_else(|| Error::Custom("no Address".into()))?,
            dns: Some(dns),
            obfuscation,
        })
    }
}

// =====================
// Convert to runtime settings
// =====================

impl TryFrom<AmneziaWgServerConfig> for AmneziaWgSettings {
    type Error = Error;

    fn try_from(cfg: AmneziaWgServerConfig) -> Result<Self, Error> {
        let address = cfg
            .address
            .parse::<IpAddrMask>()
            .map_err(|_| Error::Custom("Invalid AWG address".into()))?;

        let dns = cfg.dns.unwrap_or_default().into_iter().collect();

        let keys = WgKeys {
            privkey: cfg.private_key,
        };

        Ok(Self {
            interface: AwgInterfaceConfig {
                interface: cfg.interface,
                address,
                listen_port: cfg.port,
                private_key: keys,
                dns,
            },
            obfuscation: cfg.obfuscation,
        })
    }
}
