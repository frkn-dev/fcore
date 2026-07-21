mod aggregator;
mod common;
mod config;
mod geoip;
mod parser;
mod web_storage;

use std::io::{BufRead, BufReader};

use crate::aggregator::Aggregator;
use crate::config::AgentConfig;
use crate::geoip::GeoIpResolver;
use crate::parser::LogParser;
use crate::web_storage::WebMetricStorage;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = std::env::args()
        .nth(1)
        .expect("Usage: pixel-agent-backfill <config.toml> [<log-file-or-stdin>]");
    let log_source = std::env::args().nth(2).unwrap_or_else(|| "-".to_string());

    let config = AgentConfig::from_file(&config_path);
    let parser = LogParser::new();
    let geoip = GeoIpResolver::new(&config.geoip_db);
    let mut aggregator = Aggregator::new(config.bucket_minutes);

    let storage = WebMetricStorage::load_snapshot(
        &config.snapshot_path,
        config.max_points,
        config.retention_seconds,
    )
    .await
    .unwrap_or_else(|err| {
        eprintln!("Starting with empty storage: {}", err);
        WebMetricStorage::new(config.max_points, config.retention_seconds)
    });

    let reader: Box<dyn BufRead> = if log_source == "-" {
        Box::new(BufReader::new(std::io::stdin()))
    } else if log_source.ends_with(".gz") {
        let file = std::fs::File::open(&log_source)?;
        let decoder = flate2::read::GzDecoder::new(file);
        Box::new(BufReader::new(decoder))
    } else {
        let file = std::fs::File::open(&log_source)?;
        Box::new(BufReader::new(file))
    };

    let mut total_lines = 0u64;
    let mut parsed_events = 0u64;

    for line in reader.lines() {
        let line = line?;
        total_lines += 1;

        if let Some(event) = parser.parse_pixel_event(&line) {
            parsed_events += 1;
            aggregator.record(event, &geoip);
        }
    }

    let samples = aggregator.flush(config.retention_hours);
    let sample_count = samples.len();
    for sample in samples {
        storage.insert_sample(sample.into());
    }

    storage.save_snapshot(&config.snapshot_path).await?;

    println!(
        "Backfill complete: {} lines read, {} events parsed, {} metric samples inserted, snapshot saved to {:?}",
        total_lines, parsed_events, sample_count, config.snapshot_path
    );

    Ok(())
}
