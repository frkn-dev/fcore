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
