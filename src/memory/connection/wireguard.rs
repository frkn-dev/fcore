use base64::{engine::general_purpose, Engine};
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    error, fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    str::FromStr,
};

#[cfg(feature = "amnezia-wg")]
use netlink_packet_amnezia_wireguard::{
    AmneziaWireguardAddressFamily, AmneziaWireguardAllowedIp, AmneziaWireguardAllowedIpAttr,
};

use rand::rngs::OsRng;
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::error::Error;

#[derive(
    Archive, Clone, Debug, Serialize, Deserialize, PartialEq, RkyvDeserialize, RkyvSerialize,
)]
#[archive(check_bytes)]
pub struct Keys {
    pub privkey: String,
}

impl Default for Keys {
    fn default() -> Self {
        let secret = StaticSecret::random_from_rng(OsRng);
        let privkey_bytes = secret.as_bytes();

        Self {
            privkey: general_purpose::STANDARD.encode(privkey_bytes),
        }
    }
}

impl Keys {
    pub fn pubkey(&self) -> Result<String, Error> {
        Self::derive_pubkey(&self.privkey)
    }

    fn derive_pubkey(private_key_b64: &str) -> Result<String, Error> {
        let private_vec = general_purpose::STANDARD
            .decode(private_key_b64)
            .map_err(|e| Error::Custom(format!("invalid base64 private key: {}", e)))?;

        let private_bytes: [u8; 32] = private_vec
            .try_into()
            .map_err(|_| Error::Custom("Private key must be exactly 32 bytes".to_string()))?;

        let secret = StaticSecret::from(private_bytes);
        let public = PublicKey::from(&secret);

        Ok(general_purpose::STANDARD.encode(public.as_bytes()))
    }
}

#[derive(Clone, Copy)]
pub enum IpVersion {
    IPv4,
    IPv6,
}

#[derive(Archive, Clone, Debug, Serialize, RkyvDeserialize, RkyvSerialize, PartialEq, Eq, Hash)]
#[archive(check_bytes)]
pub struct IpAddrMask {
    // IP v4 or v6
    pub address: IpAddr,
    // Classless Inter-Domain Routing
    pub cidr: u8,
}

impl IpAddrMask {
    pub fn new(address: IpAddr, cidr: u8) -> Self {
        Self { address, cidr }
    }

    pub fn host(address: IpAddr) -> Self {
        let cidr = match address {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        Self { address, cidr }
    }

    pub fn as_ipv4(&self) -> Option<Ipv4Addr> {
        match self.address {
            IpAddr::V4(ip) => Some(ip),
            _ => None,
        }
    }

    pub fn first_peer_ip(&self) -> Option<Ipv4Addr> {
        let base = self.first_ipv4()?;
        Self::increment_ipv4(base).and_then(|ip| {
            if self.contains_ipv4(ip) {
                Some(ip)
            } else {
                None
            }
        })
    }

    pub fn first_ipv4(&self) -> Option<Ipv4Addr> {
        let base = self.as_ipv4()?;
        Self::increment_ipv4(base)
    }

    pub fn increment_ipv4(ip: Ipv4Addr) -> Option<Ipv4Addr> {
        let next = u32::from(ip).checked_add(1)?;
        Some(Ipv4Addr::from(next))
    }

    pub fn last_ipv4(&self) -> Option<Ipv4Addr> {
        self.as_ipv4()
    }

    pub fn contains_ipv4(&self, ip: Ipv4Addr) -> bool {
        match self.address {
            IpAddr::V4(base) => {
                let mask = if self.cidr == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.cidr)
                };

                (u32::from(base) & mask) == (u32::from(ip) & mask)
            }
            _ => false,
        }
    }

    /// Returns broadcast address as `IpAddr`.
    /// Note: IPv6 does not really use broadcast.
    pub fn broadcast(&self) -> IpAddr {
        match self.address {
            IpAddr::V4(ip) => {
                let addr = u32::from(ip);
                let bits = if self.cidr >= 32 {
                    0
                } else {
                    u32::MAX >> self.cidr
                };
                IpAddr::V4(Ipv4Addr::from(addr | bits))
            }
            IpAddr::V6(ip) => {
                let addr = u128::from(ip);
                let bits = if self.cidr >= 128 {
                    0
                } else {
                    u128::MAX >> self.cidr
                };
                IpAddr::V6(Ipv6Addr::from(addr | bits))
            }
        }
    }

    pub fn mask(&self) -> IpAddr {
        match self.address {
            IpAddr::V4(_) => {
                let mask = if self.cidr == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.cidr)
                };
                IpAddr::V4(Ipv4Addr::from(mask))
            }
            IpAddr::V6(_) => {
                let mask = if self.cidr == 0 {
                    0
                } else {
                    u128::MAX << (128 - self.cidr)
                };
                IpAddr::V6(Ipv6Addr::from(mask))
            }
        }
    }

    /// Returns `true` if the address defines a host, `false` if it is a network.
    pub fn is_host(&self) -> bool {
        if self.address.is_ipv4() {
            self.cidr == 32
        } else {
            self.cidr == 128
        }
    }

    /// Returns `IpVersion` for this address.
    pub fn ip_version(&self) -> IpVersion {
        if self.address.is_ipv4() {
            IpVersion::IPv4
        } else {
            IpVersion::IPv6
        }
    }
}

impl fmt::Display for IpAddrMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.address, self.cidr)
    }
}

#[derive(Debug, PartialEq)]
pub struct IpAddrParseError;

impl error::Error for IpAddrParseError {}

impl fmt::Display for IpAddrParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "IP address/mask parse error")
    }
}

impl FromStr for IpAddrMask {
    type Err = IpAddrParseError;

    fn from_str(ip_str: &str) -> Result<Self, Self::Err> {
        if let Some((left, right)) = ip_str.split_once('/') {
            let ip = left.parse().map_err(|_| IpAddrParseError)?;
            let cidr = right.parse().map_err(|_| IpAddrParseError)?;
            let max_cidr = match ip {
                IpAddr::V4(_) => 32,
                IpAddr::V6(_) => 128,
            };
            if cidr > max_cidr {
                return Err(IpAddrParseError);
            }
            Ok(IpAddrMask { address: ip, cidr })
        } else {
            let ip = ip_str.parse().map_err(|_| IpAddrParseError)?;
            Ok(IpAddrMask {
                address: ip,
                cidr: if ip.is_ipv4() { 32 } else { 128 },
            })
        }
    }
}

#[derive(
    Archive, Clone, Debug, Serialize, Deserialize, RkyvDeserialize, RkyvSerialize, PartialEq,
)]
#[archive(check_bytes)]
pub struct Param {
    pub keys: Keys,
    pub address: IpAddrMask,
}

impl Param {
    pub fn new(ip: IpAddrMask) -> Self {
        Self {
            keys: Keys::default(),
            address: ip,
        }
    }
}

impl<'de> Deserialize<'de> for IpAddrMask {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;

        struct IpAddrMaskVisitor;

        impl<'de> Visitor<'de> for IpAddrMaskVisitor {
            type Value = IpAddrMask;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("string like '10.0.0.1/24' or object with 'address' and 'cidr'")
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                s.parse().map_err(de::Error::custom)
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut address = None;
                let mut cidr = None;

                while let Some(key) = map.next_key()? {
                    match key {
                        "address" => {
                            address = Some(map.next_value()?);
                        }
                        "cidr" => {
                            cidr = Some(map.next_value()?);
                        }
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                        }
                    }
                }

                let address = address.ok_or_else(|| de::Error::missing_field("address"))?;
                let cidr = cidr.ok_or_else(|| de::Error::missing_field("cidr"))?;

                Ok(IpAddrMask { address, cidr })
            }
        }

        deserializer.deserialize_any(IpAddrMaskVisitor)
    }
}

impl Default for IpAddrMask {
    fn default() -> Self {
        "10.0.0.0/8".parse().expect("valid default network")
    }
}

#[cfg(feature = "amnezia-wg")]
impl From<IpAddrMask> for AmneziaWireguardAllowedIp {
    fn from(ip: IpAddrMask) -> Self {
        let family = match ip.address {
            std::net::IpAddr::V4(_) => AmneziaWireguardAddressFamily::Ipv4,
            std::net::IpAddr::V6(_) => AmneziaWireguardAddressFamily::Ipv6,
        };

        AmneziaWireguardAllowedIp(vec![
            AmneziaWireguardAllowedIpAttr::Family(family),
            AmneziaWireguardAllowedIpAttr::IpAddr(ip.address),
            AmneziaWireguardAllowedIpAttr::Cidr(ip.cidr),
        ])
    }
}
