use rkyv::Deserialize;
use std::sync::Arc;

use super::postgres::metrics::MetricDbBuffer;
use fcore::{MetricEnvelope, MetricStorage, Subscriber};

pub struct MetricWorker;

impl MetricWorker {
    pub async fn start(
        metric_storage: Arc<MetricStorage>,
        db_buffer: Arc<MetricDbBuffer>,
        subscriber: Subscriber,
    ) {
        tracing::info!("MetricWorker: Monitoring pipeline initialized...");

        loop {
            if let Some((_topic, payload_bytes)) = subscriber.recv().await {
                let archived =
                    match rkyv::check_archived_root::<Vec<MetricEnvelope>>(&payload_bytes) {
                        Ok(a) => a,
                        Err(e) => {
                            tracing::error!("MetricWorker: Binary corruption detected: {:?}", e);
                            continue;
                        }
                    };

                let metrics: Vec<MetricEnvelope> = match archived.deserialize(&mut rkyv::Infallible)
                {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::error!("MetricWorker: Failed to reconstruct envelopes: {:?}", e);
                        continue;
                    }
                };

                for metric in metrics {
                    tracing::debug!(
                        target: "metrics",
                        "Incoming: node={} metric={} value={} tags={:?}",
                        metric.node_id,
                        metric.name,
                        metric.value,
                        metric.tags
                    );

                    metric_storage.insert_envelope(metric.clone());
                    db_buffer.push(metric.clone());
                }
            }
        }
    }
}
