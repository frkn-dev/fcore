use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) mod impls;
pub(crate) mod snapshot;
pub(crate) mod storage;
pub(crate) mod web_storage;

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Serialize, Deserialize, Clone, Debug)]
pub struct MetricPoint {
    #[serde(rename = "x")]
    pub timestamp: i64,
    #[serde(rename = "y")]
    pub value: f64,
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Serialize, Deserialize, Clone, Debug)]
#[archive(check_bytes)]
pub struct MetricEnvelope {
    pub node_id: uuid::Uuid,
    pub name: String,
    pub value: f64,
    pub timestamp: i64,
    pub tags: BTreeMap<String, String>,
}

pub trait Metrics {
    fn heartbeat(&self) -> impl std::future::Future<Output = ()> + Send;
    fn memory(&self) -> impl std::future::Future<Output = ()> + Send;
    fn bandwidth(&self) -> impl std::future::Future<Output = ()> + Send;
    fn cpu_usage(&self) -> impl std::future::Future<Output = ()> + Send;
    fn loadavg(&self) -> impl std::future::Future<Output = ()> + Send;
    fn disk_usage(&self) -> impl std::future::Future<Output = ()> + Send;
}
