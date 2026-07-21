use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct MetricPoint {
    pub timestamp: i64,
    pub value: f64,
}

#[allow(dead_code)]
pub fn level_from_settings(level: &str) -> tracing_subscriber::EnvFilter {
    let level = match level.to_lowercase().as_str() {
        "trace" => tracing::Level::TRACE,
        "debug" => tracing::Level::DEBUG,
        "warn" => tracing::Level::WARN,
        "error" => tracing::Level::ERROR,
        _ => tracing::Level::INFO,
    };
    tracing_subscriber::EnvFilter::from_default_env()
        .add_directive(level.into())
}
