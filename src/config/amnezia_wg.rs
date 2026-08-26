use serde::{de, Deserialize, Deserializer, Serialize};
use std::net::Ipv4Addr;

use crate::{error::Error, WgKeys};

use crate::memory::connection::wireguard::IpAddrMask;

// =====================
// Obfuscation params
// =====================

/// AWG 2.0 obfuscation parameters.
///
/// `jc/jmin/jmax/s1/s2/s3/s4` are 16-bit values used by the kernel.
/// `h1..h4` and `i1..i5` are string descriptors (single values or ranges)
/// as accepted by the AmneziaWG 2.0 kernel module.
///
/// `random_trailers`/`disable_cookies` are AmneziaWG 3.1 device flags.
/// They are optional: when unset, nothing is sent over netlink and nothing
/// is emitted into client configs, so nodes running older kernel modules
/// keep working. Setting them requires the 3.1+ kernel module on the node.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AwgObfuscationParams {
    pub jc: u16,
    pub jmin: u16,
    pub jmax: u16,

    pub s1: u16,
    pub s2: u16,
    pub s3: u16,
    pub s4: u16,

    pub h1: String,
    pub h2: String,
    pub h3: String,
    pub h4: String,

    pub i1: String,
    pub i2: String,
    pub i3: String,
    pub i4: String,
    pub i5: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub random_trailers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_cookies: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum U16OrString {
    Num(u64),
    Str(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrNum {
    Str(String),
    Num(i64),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BoolOrString {
    Bool(bool),
    Str(String),
}

/// Parses an AmneziaWG-style bool: `on`/`off`, `true`/`false`, `1`/`0`.
fn parse_awg_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" => Some(true),
        "off" | "false" | "0" => Some(false),
        _ => None,
    }
}

fn parse_bool_field<'de, V>(map: &mut V) -> Result<Option<bool>, V::Error>
where
    V: de::MapAccess<'de>,
{
    match map.next_value::<Option<BoolOrString>>()? {
        None => Ok(None),
        Some(BoolOrString::Bool(b)) => Ok(Some(b)),
        Some(BoolOrString::Str(s)) => parse_awg_bool(&s)
            .map(Some)
            .ok_or_else(|| de::Error::custom(format!("invalid bool value: {s:?}"))),
    }
}

fn parse_u16_field<'de, V>(map: &mut V) -> Result<u16, V::Error>
where
    V: de::MapAccess<'de>,
{
    match map.next_value::<U16OrString>()? {
        U16OrString::Num(n) => n.try_into().map_err(|_| de::Error::custom("u16 overflow")),
        U16OrString::Str(s) => s.parse().map_err(de::Error::custom),
    }
}

fn parse_string_field<'de, V>(map: &mut V) -> Result<String, V::Error>
where
    V: de::MapAccess<'de>,
{
    match map.next_value::<StringOrNum>()? {
        StringOrNum::Str(s) => Ok(s),
        StringOrNum::Num(n) => Ok(n.to_string()),
    }
}

impl<'de> Deserialize<'de> for AwgObfuscationParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AwgObfuscationParamsVisitor;

        impl<'de> de::Visitor<'de> for AwgObfuscationParamsVisitor {
            type Value = AwgObfuscationParams;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("struct AwgObfuscationParams")
            }

            fn visit_map<V>(self, mut map: V) -> Result<AwgObfuscationParams, V::Error>
            where
                V: de::MapAccess<'de>,
            {
                let mut jc = None;
                let mut jmin = None;
                let mut jmax = None;

                let mut s1 = None;
                let mut s2 = None;
                let mut s3 = None;
                let mut s4 = None;

                let mut h1 = None;
                let mut h2 = None;
                let mut h3 = None;
                let mut h4 = None;

                let mut i1 = None;
                let mut i2 = None;
                let mut i3 = None;
                let mut i4 = None;
                let mut i5 = None;

                let mut random_trailers = None;
                let mut disable_cookies = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "jc" => jc = Some(parse_u16_field(&mut map)?),
                        "jmin" => jmin = Some(parse_u16_field(&mut map)?),
                        "jmax" => jmax = Some(parse_u16_field(&mut map)?),

                        "s1" => s1 = Some(parse_u16_field(&mut map)?),
                        "s2" => s2 = Some(parse_u16_field(&mut map)?),
                        "s3" => s3 = Some(parse_u16_field(&mut map)?),
                        "s4" => s4 = Some(parse_u16_field(&mut map)?),

                        "h1" => h1 = Some(parse_string_field(&mut map)?),
                        "h2" => h2 = Some(parse_string_field(&mut map)?),
                        "h3" => h3 = Some(parse_string_field(&mut map)?),
                        "h4" => h4 = Some(parse_string_field(&mut map)?),

                        "i1" => i1 = Some(parse_string_field(&mut map)?),
                        "i2" => i2 = Some(parse_string_field(&mut map)?),
                        "i3" => i3 = Some(parse_string_field(&mut map)?),
                        "i4" => i4 = Some(parse_string_field(&mut map)?),
                        "i5" => i5 = Some(parse_string_field(&mut map)?),

                        "random_trailers" => {
                            random_trailers = parse_bool_field(&mut map)?;
                        }
                        "disable_cookies" => {
                            disable_cookies = parse_bool_field(&mut map)?;
                        }

                        _ => {
                            // Ignore unknown fields to stay forward-compatible.
                            let _ = map.next_value::<de::IgnoredAny>()?;
                        }
                    }
                }

                Ok(AwgObfuscationParams {
                    jc: jc.unwrap_or(0),
                    jmin: jmin.unwrap_or(0),
                    jmax: jmax.unwrap_or(0),

                    s1: s1.unwrap_or(0),
                    s2: s2.unwrap_or(0),
                    s3: s3.unwrap_or(0),
                    s4: s4.unwrap_or(0),

                    h1: h1.unwrap_or_default(),
                    h2: h2.unwrap_or_default(),
                    h3: h3.unwrap_or_default(),
                    h4: h4.unwrap_or_default(),

                    i1: i1.unwrap_or_default(),
                    i2: i2.unwrap_or_default(),
                    i3: i3.unwrap_or_default(),
                    i4: i4.unwrap_or_default(),
                    i5: i5.unwrap_or_default(),

                    random_trailers,
                    disable_cookies,
                })
            }
        }

        const FIELDS: &[&str] = &[
            "jc", "jmin", "jmax", "s1", "s2", "s3", "s4", "h1", "h2", "h3", "h4", "i1", "i2", "i3",
            "i4", "i5", "random_trailers", "disable_cookies",
        ];
        deserializer.deserialize_struct("AwgObfuscationParams", FIELDS, AwgObfuscationParamsVisitor)
    }
}

// =====================
// Interface config
// =====================

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AwgInterfaceConfig {
    pub interface: String,
    pub address: IpAddrMask,
    pub listen_port: u16,
    pub mtu: Option<u16>,
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
    /// PersistentKeepalive (seconds) for client configs, set from the node's
    /// config.toml ([awg]/[awg_mobile] keepalive). None = clients get the
    /// default 25.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keepalive: Option<u16>,
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
    pub mtu: Option<u16>,

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
        let mut mtu: Option<u16> = None;

        let mut jc: Option<u16> = None;
        let mut jmin: Option<u16> = None;
        let mut jmax: Option<u16> = None;
        let mut s1: Option<u16> = None;
        let mut s2: Option<u16> = None;
        let mut s3: Option<u16> = None;
        let mut s4: Option<u16> = None;
        let mut h1: Option<String> = None;
        let mut h2: Option<String> = None;
        let mut h3: Option<String> = None;
        let mut h4: Option<String> = None;
        let mut i1: Option<String> = None;
        let mut i2: Option<String> = None;
        let mut i3: Option<String> = None;
        let mut i4: Option<String> = None;
        let mut i5: Option<String> = None;
        let mut random_trailers: Option<bool> = None;
        let mut disable_cookies: Option<bool> = None;

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

                "MTU" => {
                    mtu = value.parse::<u16>().ok();
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
                "S3" => s3 = value.parse().ok(),
                "S4" => s4 = value.parse().ok(),
                "H1" => h1 = Some(value.to_string()),
                "H2" => h2 = Some(value.to_string()),
                "H3" => h3 = Some(value.to_string()),
                "H4" => h4 = Some(value.to_string()),
                "I1" => i1 = Some(value.to_string()),
                "I2" => i2 = Some(value.to_string()),
                "I3" => i3 = Some(value.to_string()),
                "I4" => i4 = Some(value.to_string()),
                "I5" => i5 = Some(value.to_string()),
                "RandomTrailers" => random_trailers = parse_awg_bool(value),
                "DisableCookies" => disable_cookies = parse_awg_bool(value),

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
                s3: s3.unwrap_or(0),
                s4: s4.unwrap_or(0),
                h1: h1.unwrap_or_default(),
                h2: h2.unwrap_or_default(),
                h3: h3.unwrap_or_default(),
                h4: h4.unwrap_or_default(),
                i1: i1.unwrap_or_default(),
                i2: i2.unwrap_or_default(),
                i3: i3.unwrap_or_default(),
                i4: i4.unwrap_or_default(),
                i5: i5.unwrap_or_default(),
                random_trailers,
                disable_cookies,
            }),
            None => None,
        };

        Ok(Self {
            interface,
            port: port.ok_or_else(|| Error::Custom("no ListenPort".into()))?,
            private_key: private_key.ok_or_else(|| Error::Custom("no PrivateKey".into()))?,
            address: address.ok_or_else(|| Error::Custom("no Address".into()))?,
            dns: Some(dns),
            mtu,
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
                mtu: cfg.mtu,
                private_key: keys,
                dns,
            },
            obfuscation: cfg.obfuscation,
            keepalive: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_legacy_numeric_obfuscation() {
        let json = r#"{
            "jc": 6,
            "jmin": 76,
            "jmax": 169,
            "s1": 92,
            "s2": 115,
            "h1": 61220074,
            "h2": 605047389,
            "h3": 1477291385,
            "h4": 1951993942
        }"#;

        let params: AwgObfuscationParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.jc, 6);
        assert_eq!(params.jmin, 76);
        assert_eq!(params.jmax, 169);
        assert_eq!(params.s1, 92);
        assert_eq!(params.s2, 115);
        assert_eq!(params.h1, "61220074");
        assert_eq!(params.h2, "605047389");
        assert_eq!(params.h3, "1477291385");
        assert_eq!(params.h4, "1951993942");
        assert!(params.i1.is_empty());
    }

    #[test]
    fn deserialize_awg20_string_obfuscation() {
        let json = r#"{
            "jc": 6,
            "jmin": 76,
            "jmax": 169,
            "s1": 92,
            "s2": 115,
            "s3": 44,
            "s4": 9,
            "h1": "61220074-118999195",
            "h2": "605047389-945520346",
            "h3": "1477291385-1814368140",
            "h4": "1951993942-1997499713",
            "i1": "<r 149>",
            "i2": "0",
            "i3": "0",
            "i4": "0",
            "i5": "0"
        }"#;

        let params: AwgObfuscationParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.jc, 6);
        assert_eq!(params.s3, 44);
        assert_eq!(params.s4, 9);
        assert_eq!(params.h1, "61220074-118999195");
        assert_eq!(params.i1, "<r 149>");
        assert_eq!(params.i5, "0");
    }

    #[test]
    fn deserialize_missing_awg31_flags_default_to_none() {
        let json = r#"{"jc": 6, "jmin": 76, "jmax": 169}"#;

        let params: AwgObfuscationParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.random_trailers, None);
        assert_eq!(params.disable_cookies, None);

        // None flags are not serialized back, keeping stored JSON stable.
        let serialized = serde_json::to_string(&params).unwrap();
        assert!(!serialized.contains("random_trailers"));
        assert!(!serialized.contains("disable_cookies"));
    }

    #[test]
    fn deserialize_awg31_flags() {
        let json = r#"{
            "jc": 6,
            "random_trailers": true,
            "disable_cookies": "off"
        }"#;

        let params: AwgObfuscationParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.random_trailers, Some(true));
        assert_eq!(params.disable_cookies, Some(false));

        let serialized = serde_json::to_string(&params).unwrap();
        assert!(serialized.contains("\"random_trailers\":true"));
        assert!(serialized.contains("\"disable_cookies\":false"));
    }

    #[test]
    fn from_file_parses_awg31_flags() {
        let path = std::env::temp_dir().join("fcore-awg31-test.conf");
        std::fs::write(
            &path,
            "[Interface]\n\
             PrivateKey = priv\n\
             Address = 10.0.0.1/24\n\
             ListenPort = 51820\n\
             Jc = 6\n\
             Jmin = 76\n\
             Jmax = 169\n\
             RandomTrailers = on\n\
             DisableCookies = off\n",
        )
        .unwrap();

        let cfg = AmneziaWgServerConfig::from_file(path.to_str().unwrap()).unwrap();
        std::fs::remove_file(&path).ok();

        let obf = cfg.obfuscation.unwrap();
        assert_eq!(obf.random_trailers, Some(true));
        assert_eq!(obf.disable_cookies, Some(false));
    }

    #[test]
    fn from_file_without_awg31_flags_keeps_them_unset() {
        let path = std::env::temp_dir().join("fcore-awg30-test.conf");
        std::fs::write(
            &path,
            "[Interface]\n\
             PrivateKey = priv\n\
             Address = 10.0.0.1/24\n\
             ListenPort = 51820\n\
             Jc = 6\n",
        )
        .unwrap();

        let cfg = AmneziaWgServerConfig::from_file(path.to_str().unwrap()).unwrap();
        std::fs::remove_file(&path).ok();

        let obf = cfg.obfuscation.unwrap();
        assert_eq!(obf.random_trailers, None);
        assert_eq!(obf.disable_cookies, None);
    }
}
