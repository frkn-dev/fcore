use serde::Deserialize;
use serde::Serialize;

use super::super::tag::ProtoTag as Tag;
use super::wireguard::Param as WgParam;

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

#[derive(
    Archive, Deserialize, Serialize, RkyvDeserialize, RkyvSerialize, Debug, Clone, PartialEq,
)]
pub enum Proto {
    Wireguard { param: WgParam },
    AmneziaWg { param: WgParam },
    Shadowsocks { password: String },
    Xray(Tag),
    Hysteria2 { token: uuid::Uuid },
    Mtproto { secret: String },
}

impl Proto {
    pub fn proto(&self) -> Tag {
        match self {
            Proto::Wireguard { .. } => Tag::Wireguard,
            Proto::AmneziaWg { .. } => Tag::AmneziaWg,
            Proto::Shadowsocks { .. } => Tag::Shadowsocks,
            Proto::Hysteria2 { .. } => Tag::Hysteria2,
            Proto::Xray(tag) => *tag,
            Proto::Mtproto { .. } => Tag::Mtproto,
        }
    }

    pub fn token(&self) -> Option<uuid::Uuid> {
        match self {
            Proto::Hysteria2 { token } => Some(*token),
            _ => None,
        }
    }

    pub fn new_wg(param: &WgParam) -> Self {
        Proto::Wireguard {
            param: param.clone(),
        }
    }

    pub fn new_awg(param: &WgParam) -> Self {
        Proto::AmneziaWg {
            param: param.clone(),
        }
    }

    pub fn new_ss(password: &str) -> Self {
        Proto::Shadowsocks {
            password: password.to_string(),
        }
    }
    pub fn new_hysteria2(token: &uuid::Uuid) -> Self {
        Proto::Hysteria2 { token: *token }
    }

    pub fn new_xray(tag: &Tag) -> Self {
        Proto::Xray(*tag)
    }

    pub fn is_xray(&self) -> bool {
        matches!(self, Proto::Xray(_))
    }

    pub fn is_wireguard(&self) -> bool {
        matches!(self, Proto::Wireguard { .. })
    }

    pub fn is_amneziawg(&self) -> bool {
        matches!(self, Proto::AmneziaWg { .. })
    }

    pub fn is_shadowsocks(&self) -> bool {
        matches!(self, Proto::Shadowsocks { .. })
    }

    pub fn is_hysteria2(&self) -> bool {
        matches!(self, Proto::Hysteria2 { .. })
    }

    pub fn is_mtproto(&self) -> bool {
        matches!(self, Proto::Mtproto { .. })
    }
}
