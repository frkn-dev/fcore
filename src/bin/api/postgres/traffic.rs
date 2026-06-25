use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::{Error, Result};

use super::pg::PgClientManager;

pub struct PgTraffic {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgTraffic {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn upsert_bucket(
        &self,
        connection_id: uuid::Uuid,
        subscription_id: uuid::Uuid,
        env: &str,
        period: &str,
        bucket: DateTime<Utc>,
        uplink_bytes: i64,
        downlink_bytes: i64,
    ) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .execute(
                r#"
                INSERT INTO connection_traffic
                    (connection_id, subscription_id, env, period, bucket, uplink_bytes, downlink_bytes)
                VALUES
                    ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (connection_id, period, bucket) DO UPDATE
                SET uplink_bytes = connection_traffic.uplink_bytes + EXCLUDED.uplink_bytes,
                    downlink_bytes = connection_traffic.downlink_bytes + EXCLUDED.downlink_bytes,
                    updated_at = now()
                "#,
                &[
                    &connection_id,
                    &subscription_id,
                    &env,
                    &period,
                    &bucket,
                    &uplink_bytes,
                    &downlink_bytes,
                ],
            )
            .await
            .map_err(Error::Database)?;

        Ok(())
    }

    /// Sum of all persisted daily traffic for a subscription.
    /// Daily rows hold the canonical lifetime total; monthly rows are separate
    /// aggregates so summing all periods would double-count.
    pub async fn total_for_subscription(&self, subscription_id: uuid::Uuid) -> Result<(i64, i64)> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let row = client
            .query_one(
                r#"
                SELECT
                    COALESCE(SUM(uplink_bytes)::BIGINT, 0) AS uplink,
                    COALESCE(SUM(downlink_bytes)::BIGINT, 0) AS downlink
                FROM connection_traffic
                WHERE subscription_id = $1 AND period = 'day'
                "#,
                &[&subscription_id],
            )
            .await
            .map_err(Error::Database)?;

        Ok((row.get("uplink"), row.get("downlink")))
    }

    /// Sum of all persisted daily traffic per env for a subscription.
    pub async fn env_totals_for_subscription(
        &self,
        subscription_id: uuid::Uuid,
    ) -> Result<Vec<(String, i64, i64)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT
                    env,
                    COALESCE(SUM(uplink_bytes)::BIGINT, 0) AS uplink,
                    COALESCE(SUM(downlink_bytes)::BIGINT, 0) AS downlink
                FROM connection_traffic
                WHERE subscription_id = $1 AND period = 'day'
                GROUP BY env
                "#,
                &[&subscription_id],
            )
            .await
            .map_err(Error::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| (r.get("env"), r.get("uplink"), r.get("downlink")))
            .collect())
    }

    /// Traffic history for a subscription in a given period and time range,
    /// grouped by bucket and env.
    pub async fn history(
        &self,
        subscription_id: uuid::Uuid,
        period: &str,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<Vec<(DateTime<Utc>, String, i64, i64)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT
                    bucket,
                    env,
                    COALESCE(SUM(uplink_bytes)::BIGINT, 0) AS uplink,
                    COALESCE(SUM(downlink_bytes)::BIGINT, 0) AS downlink
                FROM connection_traffic
                WHERE subscription_id = $1
                  AND period = $2
                  AND ($3::timestamptz IS NULL OR bucket >= $3)
                  AND ($4::timestamptz IS NULL OR bucket <= $4)
                GROUP BY bucket, env
                ORDER BY bucket, env
                "#,
                &[&subscription_id, &period, &from, &to],
            )
            .await
            .map_err(Error::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.get("bucket"),
                    r.get("env"),
                    r.get("uplink"),
                    r.get("downlink"),
                )
            })
            .collect())
    }

    /// Aggregated traffic for a subscription in one period bucket, grouped by env.
    pub async fn env_breakdown(
        &self,
        subscription_id: uuid::Uuid,
        period: &str,
        bucket: DateTime<Utc>,
    ) -> Result<Vec<(String, i64, i64)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT
                    env,
                    COALESCE(SUM(uplink_bytes)::BIGINT, 0) AS uplink,
                    COALESCE(SUM(downlink_bytes)::BIGINT, 0) AS downlink
                FROM connection_traffic
                WHERE subscription_id = $1 AND period = $2 AND bucket = $3
                GROUP BY env
                "#,
                &[&subscription_id, &period, &bucket],
            )
            .await
            .map_err(Error::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| (r.get("env"), r.get("uplink"), r.get("downlink")))
            .collect())
    }
}
