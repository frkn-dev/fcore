//! События watchcat: переключения маршрутов и поломки перехвата.
//!
//! watchcat сам никому не пишет — он шлёт сигнал в api, а api решает, кого
//! и когда оповещать. Здесь только факты: что случилось, на какой ноде,
//! в какой группе выходов и когда.

use chrono::{DateTime, Utc};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::Result;

use super::pg::PgClientManager;

pub struct PgWatchcat {
    pub manager: Arc<Mutex<PgClientManager>>,
}

/// Одно событие от watchcat.
#[derive(Debug, Clone)]
pub struct WatchcatEvent {
    pub hostname: String,
    pub group_tag: String,
    pub kind: String,
    pub from_tag: Option<String>,
    pub to_tag: Option<String>,
    pub reason: Option<String>,
    pub detail: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

impl PgWatchcat {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn ensure_table(&self) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS watchcat_events (
                    id          bigserial PRIMARY KEY,
                    hostname    text NOT NULL,
                    group_tag   text NOT NULL,
                    kind        text NOT NULL,
                    from_tag    text,
                    to_tag      text,
                    reason      text,
                    detail      text,
                    occurred_at timestamptz NOT NULL,
                    received_at timestamptz NOT NULL DEFAULT now(),
                    notified    boolean NOT NULL DEFAULT false
                );

                -- Ретрай watchcat не должен плодить дубли: одно и то же
                -- событие (нода, группа, тип, момент) храним один раз.
                CREATE UNIQUE INDEX IF NOT EXISTS watchcat_events_dedup
                    ON watchcat_events (hostname, group_tag, kind, occurred_at);

                CREATE INDEX IF NOT EXISTS watchcat_events_group_time
                    ON watchcat_events (group_tag, occurred_at DESC);
                CREATE INDEX IF NOT EXISTS watchcat_events_time
                    ON watchcat_events (occurred_at DESC);
                "#,
            )
            .await?;

        Ok(())
    }

    /// Записывает событие. `Ok(false)` — такое уже было (ретрай).
    pub async fn record(&self, event: &WatchcatEvent) -> Result<bool> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let inserted = client
            .execute(
                r#"
                INSERT INTO watchcat_events
                    (hostname, group_tag, kind, from_tag, to_tag, reason, detail, occurred_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                ON CONFLICT (hostname, group_tag, kind, occurred_at) DO NOTHING
                "#,
                &[
                    &event.hostname,
                    &event.group_tag,
                    &event.kind,
                    &event.from_tag,
                    &event.to_tag,
                    &event.reason,
                    &event.detail,
                    &event.occurred_at,
                ],
            )
            .await?;

        Ok(inserted > 0)
    }

    pub async fn mark_notified(&self, event: &WatchcatEvent) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .execute(
                r#"
                UPDATE watchcat_events SET notified = true
                WHERE hostname = $1 AND group_tag = $2 AND kind = $3 AND occurred_at = $4
                "#,
                &[
                    &event.hostname,
                    &event.group_tag,
                    &event.kind,
                    &event.occurred_at,
                ],
            )
            .await?;

        Ok(())
    }

    /// Последние события, новые сверху.
    pub async fn recent(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT hostname, group_tag, kind, from_tag, to_tag, reason, detail,
                       occurred_at, notified
                FROM watchcat_events
                ORDER BY occurred_at DESC
                LIMIT $1
                "#,
                &[&limit],
            )
            .await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let occurred_at: DateTime<Utc> = row.get("occurred_at");
                json!({
                    "hostname": row.get::<_, String>("hostname"),
                    "group": row.get::<_, String>("group_tag"),
                    "kind": row.get::<_, String>("kind"),
                    "from": row.get::<_, Option<String>>("from_tag"),
                    "to": row.get::<_, Option<String>>("to_tag"),
                    "reason": row.get::<_, Option<String>>("reason"),
                    "detail": row.get::<_, Option<String>>("detail"),
                    "at": occurred_at.to_rfc3339(),
                    "notified": row.get::<_, bool>("notified"),
                })
            })
            .collect())
    }
}
