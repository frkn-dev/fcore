use rkyv::Archive;
use rkyv::{Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use std::fmt;
use tokio_postgres::types::FromSql;
use tokio_postgres::types::ToSql;

#[derive(
    Archive,
    Clone,
    Debug,
    RkyvDeserialize,
    RkyvSerialize,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    Hash,
    Copy,
    ToSql,
    FromSql,
)]
#[archive_attr(derive(Clone, Debug))]
#[archive(check_bytes)]
#[postgres(name = "proto", rename_all = "snake_case")]
pub enum ProtoTag {
    #[serde(rename = "VlessTcpReality")]
    VlessTcpReality,
    #[serde(rename = "VlessGrpcReality")]
    VlessGrpcReality,
    #[serde(rename = "VlessXhttpReality")]
    VlessXhttpReality,
    #[serde(rename = "VlessXhttpCdn")]
    VlessXhttpCdn,
    #[serde(rename = "Vmess")]
    Vmess,
    #[serde(rename = "Shadowsocks")]
    Shadowsocks,
    #[serde(rename = "Wireguard")]
    Wireguard,
    #[serde(rename = "AmneziaWg")]
    AmneziaWg,
    #[serde(rename = "Hysteria2")]
    Hysteria2,
    #[serde(rename = "Mtproto")]
    Mtproto,
    // NOTE: keep AmneziaWgMobile LAST — rkyv discriminants follow declaration
    // order, so appending preserves binary compatibility with older nodes/api
    // for all pre-existing protocols.
    #[serde(rename = "AmneziaWgMobile")]
    AmneziaWgMobile,
}

impl fmt::Display for ProtoTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtoTag::VlessTcpReality => write!(f, "VlessTcpReality"),
            ProtoTag::VlessGrpcReality => write!(f, "VlessGrpcReality"),
            ProtoTag::VlessXhttpReality => write!(f, "VlessXhttpReality"),
            ProtoTag::VlessXhttpCdn => write!(f, "VlessXhttpCdn"),
            ProtoTag::Vmess => write!(f, "Vmess"),
            ProtoTag::Shadowsocks => write!(f, "Shadowsocks"),
            ProtoTag::Wireguard => write!(f, "Wireguard"),
            ProtoTag::AmneziaWg => write!(f, "AmneziaWg"),
            ProtoTag::Hysteria2 => write!(f, "Hysteria2"),
            ProtoTag::Mtproto => write!(f, "Mtproto"),
            ProtoTag::AmneziaWgMobile => write!(f, "AmneziaWgMobile"),
        }
    }
}

impl ProtoTag {
    pub fn is_wireguard(&self) -> bool {
        *self == ProtoTag::Wireguard
    }

    pub fn is_amneziawg(&self) -> bool {
        *self == ProtoTag::AmneziaWg
    }

    pub fn is_amneziawg_mobile(&self) -> bool {
        *self == ProtoTag::AmneziaWgMobile
    }

    pub fn is_shadowsocks(&self) -> bool {
        *self == ProtoTag::Shadowsocks
    }
    pub fn is_hysteria2(&self) -> bool {
        *self == ProtoTag::Hysteria2
    }
    pub fn is_mtproto(&self) -> bool {
        *self == ProtoTag::Mtproto
    }

    /// Whether the protocol is allowed while a subscription runs on its
    /// traffic balance (traffic mode): only protocols with per-connection
    /// traffic accounting are metered. `metered_conns` holds tag names as in
    /// the config ("Wireguard", "VlessTcpReality", ...).
    pub fn is_metered(&self, metered_conns: &[String]) -> bool {
        metered_conns.iter().any(|t| t == &self.to_string())
    }
}

impl std::str::FromStr for ProtoTag {
    type Err = ();

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "VlessTcpReality" => Ok(ProtoTag::VlessTcpReality),
            "VlessGrpcReality" => Ok(ProtoTag::VlessGrpcReality),
            "VlessXhttpReality" => Ok(ProtoTag::VlessXhttpReality),
            "VlessXhttpCdn" => Ok(ProtoTag::VlessXhttpCdn),
            "Vmess" => Ok(ProtoTag::Vmess),
            "Shadowsocks" => Ok(ProtoTag::Shadowsocks),
            "Wireguard" => Ok(ProtoTag::Wireguard),
            "AmneziaWg" => Ok(ProtoTag::AmneziaWg),
            "AmneziaWgMobile" => Ok(ProtoTag::AmneziaWgMobile),
            "Hysteria2" => Ok(ProtoTag::Hysteria2),
            "Mtproto" => Ok(ProtoTag::Mtproto),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metered() -> Vec<String> {
        [
            "Wireguard",
            "AmneziaWg",
            "AmneziaWgMobile",
            "VlessTcpReality",
            "VlessGrpcReality",
            "VlessXhttpCdn",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn test_is_metered_default_set() {
        let metered = metered();

        for tag in [
            ProtoTag::Wireguard,
            ProtoTag::AmneziaWg,
            ProtoTag::AmneziaWgMobile,
            ProtoTag::VlessTcpReality,
            ProtoTag::VlessGrpcReality,
            ProtoTag::VlessXhttpCdn,
        ] {
            assert!(tag.is_metered(&metered), "{} should be metered", tag);
        }

        // Protocols without per-connection traffic accounting are not metered.
        for tag in [ProtoTag::Hysteria2, ProtoTag::Mtproto] {
            assert!(!tag.is_metered(&metered), "{} should not be metered", tag);
        }
    }

    #[test]
    fn test_is_metered_custom_list() {
        let only_h2 = vec!["Hysteria2".to_string()];
        assert!(ProtoTag::Hysteria2.is_metered(&only_h2));
        assert!(!ProtoTag::Wireguard.is_metered(&only_h2));

        let empty: Vec<String> = vec![];
        assert!(!ProtoTag::Wireguard.is_metered(&empty));
    }
}
