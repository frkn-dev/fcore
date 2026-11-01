//! События подключения: клиент сообщил, что туннель поднялся.
//!
//! Механизма два в одном запросе: обычный `POST /v1/config` с флагом
//! `is_connect_event: true`. Отдельного эндпойнта нет — клиент и так ходит
//! за конфигом при подключении, а событие лишь добавляет к этому запросу
//! endpoint/порт/support_tag.
//!
//! Провалы сюда не пишутся: для них есть `v1/report/block`.

use chrono::{DateTime, Utc};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use fcore::Result;

use super::pg::PgClientManager;

pub struct PgConnect {
    pub manager: Arc<Mutex<PgClientManager>>,
}

/// Одно событие подключения.
#[derive(Debug, Clone)]
pub struct ConnectEvent {
    pub subscription_id: Option<Uuid>,
    pub node_id: Option<Uuid>,
    pub connection_id: Option<Uuid>,
    pub protocol: String,
    pub endpoint: Option<std::net::IpAddr>,
    pub endpoint_port: Option<i32>,
    pub support_tag: Option<Uuid>,
    pub app_version: String,
    pub platform: String,
    pub country: Option<String>,
}

impl PgConnect {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn ensure_table(&self) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS connect_events (
                    id              bigserial PRIMARY KEY,
                    at              timestamptz NOT NULL DEFAULT now(),
                    subscription_id uuid,
                    node_id         uuid,
                    connection_id   uuid,
                    protocol        text NOT NULL,
                    endpoint        inet,
                    endpoint_port   int,
                    support_tag     uuid,
                    app_version     text NOT NULL,
                    platform        text NOT NULL,
                    country         text
                );

                CREATE INDEX IF NOT EXISTS connect_events_sub_time
                    ON connect_events (subscription_id, at DESC);
                CREATE INDEX IF NOT EXISTS connect_events_tag_time
                    ON connect_events (support_tag, at DESC);
                CREATE INDEX IF NOT EXISTS connect_events_endpoint_time
                    ON connect_events (endpoint, at DESC);
                "#,
            )
            .await?;
        Ok(())
    }

    pub async fn record(&self, e: &ConnectEvent) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .execute(
                r#"
                INSERT INTO connect_events
                    (subscription_id, node_id, connection_id, protocol,
                     endpoint, endpoint_port, support_tag, app_version, platform, country)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                "#,
                &[
                    &e.subscription_id,
                    &e.node_id,
                    &e.connection_id,
                    &e.protocol,
                    &e.endpoint,
                    &e.endpoint_port,
                    &e.support_tag,
                    &e.app_version,
                    &e.platform,
                    &e.country,
                ],
            )
            .await?;
        Ok(())
    }

    /// Сводка по подписке за период: сколько подключений, по каким адресам.
    pub async fn stats(
        &self,
        subscription_id: Option<Uuid>,
        support_tag: Option<Uuid>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<serde_json::Value> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let total: i64 = client
            .query_one(
                "SELECT count(*) FROM connect_events
                 WHERE ($1::uuid IS NULL OR subscription_id = $1)
                   AND ($2::uuid IS NULL OR support_tag = $2)
                   AND at >= $3 AND at < $4",
                &[&subscription_id, &support_tag, &from, &to],
            )
            .await?
            .get(0);

        let rows = client
            .query(
                r#"
                SELECT host(endpoint) AS addr, endpoint_port, protocol,
                       count(*) AS n, max(at) AS last_at
                FROM connect_events
                WHERE ($1::uuid IS NULL OR subscription_id = $1)
                  AND ($2::uuid IS NULL OR support_tag = $2)
                  AND at >= $3 AND at < $4
                GROUP BY 1, 2, 3
                ORDER BY n DESC
                LIMIT 50
                "#,
                &[&subscription_id, &support_tag, &from, &to],
            )
            .await?;

        let by_endpoint: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                let last: Option<DateTime<Utc>> = r.get("last_at");
                json!({
                    "endpoint": r.get::<_, Option<String>>("addr"),
                    "port": r.get::<_, Option<i32>>("endpoint_port"),
                    "protocol": r.get::<_, String>("protocol"),
                    "count": r.get::<_, i64>("n"),
                    "last_at": last.map(|d| d.to_rfc3339()),
                })
            })
            .collect();

        Ok(json!({
            "subscription_id": subscription_id,
            "support_tag": support_tag,
            "from": from.to_rfc3339(),
            "to": to.to_rfc3339(),
            "connected": total,
            "by_endpoint": by_endpoint,
        }))
    }
}
