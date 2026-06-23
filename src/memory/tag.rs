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

    pub fn is_shadowsocks(&self) -> bool {
        *self == ProtoTag::Shadowsocks
    }
    pub fn is_hysteria2(&self) -> bool {
        *self == ProtoTag::Hysteria2
    }
    pub fn is_mtproto(&self) -> bool {
        *self == ProtoTag::Mtproto
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
            "Hysteria2" => Ok(ProtoTag::Hysteria2),
            "Mtproto" => Ok(ProtoTag::Mtproto),
            _ => Err(()),
        }
    }
}
