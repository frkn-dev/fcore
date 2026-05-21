use sqlx::{PgPool, Postgres, QueryBuilder};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

use fcore::{MetricEnvelope, Result};

pub struct MetricDbBuffer {
    pub batch: parking_lot::Mutex<Vec<MetricEnvelope>>,
    pub pg: Arc<PostgresMetricWriter>,
}

impl MetricDbBuffer {
    pub fn push(&self, e: MetricEnvelope) {
        let mut b = self.batch.lock();
        b.push(e);
    }
}

impl MetricDbBuffer {
    pub async fn flush_loop(&self, interval: u64) {
        loop {
            sleep(Duration::from_secs(interval)).await;

            let batch = {
                let mut b = self.batch.lock();
                if b.is_empty() {
                    continue;
                }
                std::mem::take(&mut *b)
            };

            tracing::debug!("pg flush batch size={}", batch.len());

            if let Err(e) = self.pg.write_batch(&batch).await {
                tracing::error!("metrics pg flush failed: {}", e);
            }
        }
    }
}

pub struct PostgresMetricWriter {
    pub pool: PgPool,
}

impl PostgresMetricWriter {
    pub async fn write_batch(&self, batch: &[MetricEnvelope]) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }

        const CHUNK_SIZE: usize = 2000;

        for chunk in batch.chunks(CHUNK_SIZE) {
            let mut tx = self.pool.begin().await?;

            let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
                "INSERT INTO node_metrics (time, node_id, metric, value, labels) ",
            );

            qb.push_values(chunk.iter(), |mut b, m| {
                b.push_bind(
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(m.timestamp).unwrap(),
                )
                .push_bind(m.node_id)
                .push_bind(&m.name)
                .push_bind(m.value)
                .push_bind(serde_json::to_value(&m.tags).unwrap());
            });

            tracing::debug!(
                target: "metrics",
                "pg insert chunk size={} params={}",
                chunk.len(),
                chunk.len() * 5
            );

            qb.build().execute(&mut *tx).await?;

            tx.commit().await?;
        }

        Ok(())
    }
}
