use crate::memory::tag::ProtoTag;
use chrono::{DateTime, Utc};

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDes, Serialize as SerdeSer};
use std::fmt;

use crate::memory::connection::wireguard::Param as WgParam;

#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeDes)]
#[archive(check_bytes)]
#[archive_attr(derive(Debug))]
pub struct RkyvDateTime {
    timestamp: i64,
    nanos: u32,
}

impl From<DateTime<Utc>> for RkyvDateTime {
    fn from(dt: DateTime<Utc>) -> Self {
        Self {
            timestamp: dt.timestamp(),
            nanos: dt.timestamp_subsec_nanos(),
        }
    }
}

impl From<RkyvDateTime> for DateTime<Utc> {
    fn from(rkyv_dt: RkyvDateTime) -> Self {
        DateTime::from_timestamp(rkyv_dt.timestamp, rkyv_dt.nanos).expect("Invalid timestamp")
    }
}

#[derive(Archive, Serialize, Deserialize, SerdeSer, SerdeDes, Debug, Clone)]
#[archive(check_bytes)]
pub enum Action {
    Create,
    Update,
    Delete,
    ResetStat,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Action::Create => write!(f, "Create"),
            Action::Delete => write!(f, "Delete"),
            Action::Update => write!(f, "Update"),
            Action::ResetStat => write!(f, "ResetStat"),
        }
    }
}

#[derive(Archive, Serialize, Deserialize, Clone, Debug, SerdeDes)]
#[archive(check_bytes)]
pub struct Message {
    pub conn_id: uuid::Uuid,
    pub action: Action,
    pub tag: ProtoTag,
    pub wg: Option<WgParam>,
    pub password: Option<String>,
    pub token: Option<uuid::Uuid>,
    pub expires_at: Option<RkyvDateTime>,
    pub subscription_id: Option<uuid::Uuid>,
}

/// One peer change for the node plane.
///
/// Everything the node needs to program the interface: who (public key), where
/// (address), and which device (tag). No private key, no auth data — a node
/// never decides anything about peers.
#[derive(Archive, Serialize, Deserialize, SerdeSer, SerdeDes, Debug, Clone)]
#[archive(check_bytes)]
pub struct PeerMessage {
    pub peer_id: uuid::Uuid,
    pub action: Action,
    pub tag: ProtoTag,
    pub public_key: String,
    /// Address as `a.b.c.d`, the node appends `/32`.
    pub address: String,
    pub subscription_id: Option<uuid::Uuid>,
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} | {} | {} | {} | {} | {} | {}",
            self.conn_id.clone(),
            self.action,
            self.tag,
            match &self.wg {
                Some(wg) => format!("{} | {}", wg.address, wg.keys.privkey),
                None => "-".to_string(),
            },
            match &self.password {
                Some(pw) => pw.as_ref(),
                None => "-",
            },
            match &self.token {
                Some(t) => t.to_string(),
                None => "-".to_string(),
            },
            match &self.expires_at {
                Some(exp_at) => format!("{:?}", exp_at),
                None => "-".to_string(),
            }
        )
    }
}
