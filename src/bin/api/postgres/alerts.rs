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

                -- Последний heartbeat по ноде: переживает рестарт api, чтобы
                -- правило «нода молчит» работало и после перезапуска.
                CREATE TABLE IF NOT EXISTS node_heartbeats (
                    node_id    uuid PRIMARY KEY,
                    last_ms    bigint NOT NULL,
                    updated_at timestamptz NOT NULL DEFAULT now()
                );
                "#,
            )
            .await?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    /// Ретенция истории алертов: сколько дней держим.
    pub const RETENTION_DAYS: i64 = 180;

    /// Удаляет события старше `days` дней.
    pub async fn purge_old(&self, days: i64) -> Result<u64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let deleted = client
            .execute(
                "DELETE FROM alert_events WHERE created_at < now() - ($1 || ' days')::interval",
                &[&days.to_string()],
            )
            .await?;

        Ok(deleted)
    }

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

    /// Сохраняет последние heartbeat-ы (по одной записи на ноду).
    pub async fn upsert_heartbeats(&self, rows: &[(uuid::Uuid, i64)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let ids: Vec<uuid::Uuid> = rows.iter().map(|(id, _)| *id).collect();
        let stamps: Vec<i64> = rows.iter().map(|(_, ms)| *ms).collect();

        client
            .execute(
                r#"
                INSERT INTO node_heartbeats (node_id, last_ms, updated_at)
                SELECT * FROM unnest($1::uuid[], $2::bigint[]) AS t(node_id, last_ms), now()
                ON CONFLICT (node_id) DO UPDATE SET last_ms = EXCLUDED.last_ms, updated_at = now()
                "#,
                &[&ids, &stamps],
            )
            .await?;

        Ok(())
    }

    /// Последние известные heartbeat-ы (для затравки движка после рестарта).
    pub async fn load_heartbeats(&self) -> Result<Vec<(uuid::Uuid, i64)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query("SELECT node_id, last_ms FROM node_heartbeats", &[])
            .await?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, uuid::Uuid>("node_id"),
                    r.get::<_, i64>("last_ms"),
                )
            })
            .collect())
    }

    /// Ноды, по которым последнее событие — «упала» (то есть алерт уже был и
    /// отбоя не последовало). Нужно, чтобы рестарт не спамил теми же авариями.
    pub async fn nodes_with_open_offline_alert(&self) -> Result<Vec<uuid::Uuid>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT node_id FROM (
                    SELECT DISTINCT ON (node_id) node_id, rule
                    FROM alert_events
                    WHERE node_id IS NOT NULL
                    ORDER BY node_id, created_at DESC
                ) last WHERE rule = 'node_offline'
                "#,
                &[],
            )
            .await?;

        Ok(rows
            .iter()
            .map(|r| r.get::<_, uuid::Uuid>("node_id"))
            .collect())
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
