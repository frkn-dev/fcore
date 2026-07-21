mod aggregator;
mod common;
mod config;
mod geoip;
mod parser;
mod server;
mod web_storage;

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::aggregator::Aggregator;
use crate::config::AgentConfig;
use crate::geoip::GeoIpResolver;
use crate::parser::LogParser;
use crate::web_storage::WebMetricStorage;
use crate::common::level_from_settings;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = std::env::args()
        .nth(1)
        .expect("required config path as an argument");

    let config = Arc::new(AgentConfig::from_file(&config_path));

    tracing_subscriber::fmt()
        .with_env_filter(level_from_settings(&config.log_level))
        .init();

    tracing::info!("Pixel agent started");

    let parser = LogParser::new();
    let geoip = GeoIpResolver::new(&config.geoip_db);
    let aggregator = Arc::new(Mutex::new(Aggregator::new(config.bucket_minutes)));
    let storage = Arc::new(
        WebMetricStorage::load_snapshot(
            &config.snapshot_path,
            config.max_points,
            config.retention_seconds,
        )
        .await
        .unwrap_or_else(|err| {
            tracing::warn!("Failed to load snapshot: {}, starting empty", err);
            WebMetricStorage::new(config.max_points, config.retention_seconds)
        }),
    );

    let server_aggregator = aggregator.clone();
    let server_storage = Arc::clone(&storage);
    let server_config = Arc::clone(&config);
    tokio::spawn(async move {
        server::start_admin_server(
            server_config.admin_listen.clone(),
            server_config.admin_port,
            server_config.cors_origins.clone(),
            server_aggregator,
            server_storage,
        )
        .await;
    });

    let poll_interval_sec = config.poll_interval_sec;
    let flush_interval_sec = config.flush_interval_sec;
    let snapshot_interval_sec = config.snapshot_interval_sec;
    let retention_hours = config.retention_hours;
    let snapshot_path = config.snapshot_path.clone();
    let mut poll_interval = tokio::time::interval(Duration::from_secs(poll_interval_sec));
    let mut flush_interval = tokio::time::interval(Duration::from_secs(flush_interval_sec));
    let mut snapshot_interval = tokio::time::interval(Duration::from_secs(snapshot_interval_sec));

    loop {
        tokio::select! {
            _ = poll_interval.tick() => {
                if let Err(err) = poll_log(
                    &config,
                    &parser,
                    &geoip,
                    aggregator.clone(),
                ).await {
                    tracing::error!("Log poll failed: {}", err);
                }
            }
            _ = flush_interval.tick() => {
                if let Err(err) = flush_metrics(
                    aggregator.clone(),
                    &storage,
                    retention_hours,
                ).await {
                    tracing::error!("Flush failed: {}", err);
                }
            }
            _ = snapshot_interval.tick() => {
                if let Err(err) = storage.save_snapshot(&snapshot_path).await {
                    tracing::error!("Snapshot failed: {}", err);
                }
            }
        }
    }
}

async fn poll_log(
    config: &AgentConfig,
    parser: &LogParser,
    geoip: &GeoIpResolver,
    aggregator: Arc<Mutex<Aggregator>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let offset = read_offset(&config.offset_path).await.unwrap_or(0);

    let metadata = tokio::fs::metadata(&config.log_path).await?;
    let file_size = metadata.len();

    if file_size < offset {
        tracing::warn!("Log file shrank, resetting offset");
        write_offset(&config.offset_path, 0).await?;
        return Ok(());
    }

    let file = tokio::fs::File::open(&config.log_path).await?;
    let mut reader = tokio::io::BufReader::new(file);
    use tokio::io::{AsyncBufReadExt, AsyncSeekExt};
    reader.seek(std::io::SeekFrom::Start(offset)).await?;

    let mut new_offset = offset;
    let mut buffer = String::new();

    loop {
        buffer.clear();
        let bytes_read = reader.read_line(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        new_offset += bytes_read as u64;

        if let Some(event) = parser.parse_pixel_event(&buffer) {
            let mut agg = aggregator.lock().await;
            agg.record(event, geoip);
        }
    }

    write_offset(&config.offset_path, new_offset).await?;

    tracing::debug!("Polled log up to offset {}", new_offset);
    Ok(())
}

async fn flush_metrics(
    aggregator: Arc<Mutex<Aggregator>>,
    storage: &WebMetricStorage,
    retention_hours: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let samples = {
        let mut agg = aggregator.lock().await;
        agg.flush(retention_hours)
    };

    if samples.is_empty() {
        return Ok(());
    }

    let max_ts = samples.iter().map(|s| s.timestamp_ms).max().unwrap_or(0);

    for sample in &samples {
        storage.insert_sample(sample.clone().into());
    }

    let mut agg = aggregator.lock().await;
    agg.clear_flushed(max_ts);

    tracing::info!("Flushed {} pixel metrics into local storage", samples.len());

    Ok(())
}

async fn read_offset(path: &std::path::Path) -> Option<u64> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    content.trim().parse().ok()
}

async fn write_offset(path: &std::path::Path, offset: u64) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, offset.to_string()).await?;
    Ok(())
}
