//! История срабатываний правил (`alert_events`).

use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::Result;

use super::pg::PgClientManager;

pub struct PgAlerts {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgAlerts {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn ensure_table(&self) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS alert_events (
                    id          bigserial PRIMARY KEY,
                    rule        text NOT NULL,
                    node_id     uuid,
                    hostname    text,
                    env         text,
                    duration_sec bigint,
                    delivered   boolean NOT NULL DEFAULT false,
                    details     jsonb,
                    created_at  timestamptz NOT NULL DEFAULT now()
                );

                CREATE INDEX IF NOT EXISTS alert_events_rule_time
                    ON alert_events (rule, created_at DESC);
                CREATE INDEX IF NOT EXISTS alert_events_node_time
                    ON alert_events (node_id, created_at DESC);
                "#,
            )
            .await?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        rule: &str,
        node_id: Option<uuid::Uuid>,
        hostname: Option<&str>,
        env: Option<&str>,
        duration_sec: i64,
        delivered: bool,
        details: &serde_json::Value,
        at: DateTime<Utc>,
    ) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .execute(
                r#"
                INSERT INTO alert_events
                    (rule, node_id, hostname, env, duration_sec, delivered, details, created_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                "#,
                &[
                    &rule,
                    &node_id,
                    &hostname,
                    &env,
                    &duration_sec,
                    &delivered,
                    &details,
                    &at,
                ],
            )
            .await?;

        Ok(())
    }

    /// Последние срабатывания — для отладки и будущей админки.
    pub async fn recent(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT rule, node_id, hostname, env, duration_sec, delivered, created_at
                FROM alert_events
                ORDER BY created_at DESC
                LIMIT $1
                "#,
                &[&limit],
            )
            .await?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "rule": r.get::<_, String>("rule"),
                    "node_id": r.get::<_, Option<uuid::Uuid>>("node_id"),
                    "hostname": r.get::<_, Option<String>>("hostname"),
                    "env": r.get::<_, Option<String>>("env"),
                    "duration_sec": r.get::<_, Option<i64>>("duration_sec"),
                    "delivered": r.get::<_, bool>("delivered"),
                    "at": r.get::<_, DateTime<Utc>>("created_at"),
                })
            })
            .collect())
    }
}
