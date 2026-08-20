use crate::memory::connection::conn::Conn as Connection;
use crate::memory::connection::stat::Stat as ConnectionStat;
use crate::memory::env::Env;
use crate::memory::key::Key;
use crate::memory::subscription::Subscription;
use crate::memory::tag::ProtoTag as Tag;
use serde::{Deserialize, Serialize};

use chrono::DateTime;
use chrono::Utc;

#[derive(Deserialize, Serialize)]
pub struct ResponseMessage<T> {
    pub status: u16,
    pub message: String,
    pub response: T,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct InstanceWithId<T> {
    pub id: uuid::Uuid,
    pub instance: T,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Instance {
    Connection(Connection),
    Subscription(Subscription),
    SubscriptionResponse(SubscriptionResponse),
    Stat(Vec<(uuid::Uuid, ConnectionStat, Tag)>),
    Connections(Vec<(uuid::Uuid, Connection)>),
    Key(Key),
    Count(usize),
    None,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SubscriptionResponse {
    pub id: uuid::Uuid,
    pub expires: DateTime<Utc>,
    pub days: i64,
    pub ref_code: String,
    pub locations: Vec<EnvInfo>,
    pub downlink: i64,
    pub uplink: i64,
    pub daily_downlink: i64,
    pub daily_uplink: i64,
    pub monthly_downlink: i64,
    pub monthly_uplink: i64,
    pub limit_bytes: i64,
    pub env_traffic: Vec<EnvTrafficInfo>,
    pub connections: Vec<ConnectionInfo>,
}

/// Safe projection of a connection for the subscription-info endpoint:
/// identity, routing metadata and the user-facing label only — never
/// key material, addresses or tokens. Soft-deleted connections are
/// included with `is_deleted: true` (the front hides them).
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ConnectionInfo {
    pub id: uuid::Uuid,
    pub env: Env,
    pub proto: Tag,
    pub label: Option<String>,
    pub is_deleted: bool,
    pub uplink: i64,
    pub downlink: i64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EnvTrafficInfo {
    pub env: Env,
    pub downlink: i64,
    pub uplink: i64,
    pub daily_downlink: i64,
    pub daily_uplink: i64,
    pub monthly_downlink: i64,
    pub monthly_uplink: i64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SubscriptionTrafficHistoryResponse {
    pub subscription_id: uuid::Uuid,
    pub period: String,
    pub buckets: Vec<TrafficHistoryBucket>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TrafficHistoryBucket {
    pub bucket: DateTime<Utc>,
    pub uplink: i64,
    pub downlink: i64,
    pub envs: Vec<EnvTrafficHistoryBucket>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EnvTrafficHistoryBucket {
    pub env: Env,
    pub uplink: i64,
    pub downlink: i64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EnvInfo {
    pub env: Env,
    pub has_xray: bool,
    pub has_h2: bool,
    pub has_mtproto: bool,
    pub has_wg: bool,
    pub has_awg: bool,
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_info_is_safe_projection() {
        let info = ConnectionInfo {
            id: uuid::Uuid::nil(),
            env: Env::Ru,
            proto: Tag::Wireguard,
            label: Some("Мама Андроид".to_string()),
            is_deleted: false,
            uplink: 1024,
            downlink: 2048,
        };

        let value = serde_json::to_value(&info).unwrap();
        let obj = value.as_object().unwrap();

        // Exactly these fields — the projection must never grow key
        // material (wg_privkey), addresses or tokens.
        let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            ["downlink", "env", "id", "is_deleted", "label", "proto", "uplink"]
        );

        assert_eq!(obj["proto"], serde_json::json!("Wireguard"));
        assert_eq!(obj["env"], serde_json::json!("ru"));
        assert_eq!(obj["label"], serde_json::json!("Мама Андроид"));
        assert_eq!(obj["is_deleted"], serde_json::json!(false));
        assert_eq!(obj["uplink"], serde_json::json!(1024));
        assert_eq!(obj["downlink"], serde_json::json!(2048));
    }

    #[test]
    fn test_connection_info_label_nullable() {
        let info = ConnectionInfo {
            id: uuid::Uuid::nil(),
            env: Env::Ru,
            proto: Tag::AmneziaWgMobile,
            label: None,
            is_deleted: true,
            uplink: 0,
            downlink: 0,
        };

        let value = serde_json::to_value(&info).unwrap();
        assert_eq!(value["label"], serde_json::Value::Null);
        assert_eq!(value["proto"], serde_json::json!("AmneziaWgMobile"));
        assert_eq!(value["is_deleted"], serde_json::json!(true));
    }
}
