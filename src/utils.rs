use chrono::{Duration, Local, NaiveTime, Utc};
use rand::{distributions::Alphanumeric, Rng};
use std::time::Instant;
use tokio::time::{sleep, Duration as TokioDuration};
use tracing_subscriber::EnvFilter;

/// Derive a stable, node-specific VMess credential while retaining the parent
/// connection ID for accounting and revocation.
pub fn vmess_node_uuid(connection_id: &uuid::Uuid, node_hostname: &str) -> uuid::Uuid {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"fcore-vmess-node-v1\0");
    hash.update(connection_id.as_bytes());
    hash.update(node_hostname.as_bytes());
    let digest = hash.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // Some VMess clients accept only the familiar version-4 UUID shape.
    // The value remains deterministically derived, not randomly generated.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod vmess_node_uuid_tests {
    use super::vmess_node_uuid;

    #[test]
    fn stable_and_separate_for_each_node_and_connection() {
        let first = uuid::Uuid::from_u128(1);
        let second = uuid::Uuid::from_u128(2);
        assert_eq!(vmess_node_uuid(&first, "node-a"), vmess_node_uuid(&first, "node-a"));
        assert_ne!(vmess_node_uuid(&first, "node-a"), vmess_node_uuid(&first, "node-b"));
        assert_ne!(vmess_node_uuid(&first, "node-a"), vmess_node_uuid(&second, "node-a"));
        assert_eq!(vmess_node_uuid(&first, "node-a").get_version_num(), 4);
    }
}

pub async fn run_daily<F, Fut>(task: F, target_time: NaiveTime)
where
    F: Fn() -> Fut + Send + Sync + 'static + Clone,
    Fut: std::future::Future<Output = ()> + Send,
{
    loop {
        tracing::debug!("Running daily scheduled task {}", target_time,);
        let now = Local::now();
        let today_target = now.date_naive().and_time(target_time);

        let next_run = {
            if now.time() < target_time {
                today_target
            } else {
                today_target + Duration::days(1)
            }
        };

        tracing::debug!(
            "Running daily shcduked task {}, next run - {}",
            today_target,
            next_run
        );

        let wait_duration = (next_run - now.naive_local()).to_std().unwrap_or_default();
        sleep(TokioDuration::from_secs(wait_duration.as_secs())).await;

        task().await;
    }
}

pub fn to_pg_bigint(value: u64) -> Option<i64> {
    if value <= i64::MAX as u64 {
        Some(value as i64)
    } else {
        None
    }
}

pub fn from_pg_bigint(value: i64) -> u64 {
    value as u64
}

pub fn generate_random_password(length: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
        .collect()
}

pub async fn measure_time<T, F>(task: F, name: &str) -> T
where
    F: std::future::Future<Output = T>,
{
    let start_time = Instant::now();
    let result = task.await;
    let duration = start_time.elapsed();
    tracing::debug!("Task {} completed in {:?}", name, duration);
    result
}

pub fn current_timestamp() -> i64 {
    Utc::now().timestamp()
}

pub fn round_to_two_decimal_places(value: f32) -> f32 {
    (value * 100.0).round() / 100.0
}

pub fn get_uuid_last_octet_simple(uuid: &uuid::Uuid) -> String {
    uuid.as_hyphenated().to_string()[24..].to_string()
}

pub fn level_from_settings(level: &str) -> EnvFilter {
    EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"))
}
