use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) mod impls;

pub use impls::{mark_process_start, process_uptime_secs};
pub(crate) mod snapshot;
pub(crate) mod storage;

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

/// Наблюдение о входе: с какого адреса клиент сидит на ноде.
///
/// Отдельный топик, а не метрика: IP меняется, а ключ серии в MetricStorage —
/// хеш `(имя + tags)`, поэтому каждый новый адрес плодил бы новую серию и
/// раздувал память. Здесь же это разовое событие, а не временной ряд.
#[derive(Archive, RkyvSerialize, RkyvDeserialize, Serialize, Deserialize, Clone, Debug)]
#[archive(check_bytes)]
pub struct EntryEnvelope {
    pub node_id: uuid::Uuid,
    pub conn_id: uuid::Uuid,
    pub subscription_id: Option<uuid::Uuid>,
    pub proto: String,
    /// Адрес, с которого пришёл клиент (endpoint пира).
    pub ip: String,
    pub timestamp: i64,
}

pub trait Metrics {
    fn heartbeat(&self) -> impl std::future::Future<Output = ()> + Send;
    fn memory(&self) -> impl std::future::Future<Output = ()> + Send;
    fn bandwidth(&self) -> impl std::future::Future<Output = ()> + Send;
    fn cpu_usage(&self) -> impl std::future::Future<Output = ()> + Send;
    fn loadavg(&self) -> impl std::future::Future<Output = ()> + Send;
    fn disk_usage(&self) -> impl std::future::Future<Output = ()> + Send;
}
