//! Хранилище для сводок: отметки об отправленных отчётах и обёртка над
//! `digest::Digest::collect` для одного соединения.

use std::sync::Arc;

use chrono::{DateTime, Datelike, Timelike, Utc};
use tokio::sync::Mutex;

use fcore::Result;

use super::pg::PgClientManager;
use crate::alerts::AlertEmailConfig;
use crate::digest::{self, DigestConfig, DigestKind};

pub struct PgDigest {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgDigest {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    /// Таблица отметок: одна строка на (вид отчёта, период). Она же защищает от
    /// двойной отправки после рестарта.
    pub async fn ensure_table(&self) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS digest_runs (
                    kind      text NOT NULL,
                    period    text NOT NULL,
                    sent_at   timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (kind, period)
                );
                "#,
            )
            .await?;
        Ok(())
    }

    /// `true` — отчёт за этот период ещё не отправляли и теперь он наш.
    pub async fn claim_run(&self, kind: &str, period: &str) -> Result<bool> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let inserted = client
            .execute(
                "INSERT INTO digest_runs (kind, period) VALUES ($1, $2)
                 ON CONFLICT (kind, period) DO NOTHING",
                &[&kind, &period],
            )
            .await?;
        Ok(inserted > 0)
    }

    pub async fn collect(
        &self,
        kind: DigestKind,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<digest::Digest> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        Ok(digest::Digest::collect(client, kind, from, to).await?)
    }

    /// Отправить все отчёты, которые пора отправить. Вызывается раз в 10 минут.
    pub async fn run_due(
        &self,
        cfg: &DigestConfig,
        email: &AlertEmailConfig,
    ) -> Result<Vec<String>> {
        let mut sent = Vec::new();
        if !cfg.enabled {
            return Ok(sent);
        }
        self.ensure_table().await?;

        let now = Utc::now();
        let local = cfg.local_now(now);
        if local.hour() < cfg.hour {
            return Ok(sent);
        }

        let mut due: Vec<(DigestKind, String)> = vec![(
            DigestKind::Daily,
            local.format("%Y-%m-%d").to_string(),
        )];
        if cfg.weekly && local.weekday() == chrono::Weekday::Mon {
            due.push((DigestKind::Weekly, local.format("%G-W%V").to_string()));
        }
        if cfg.monthly && local.day() == 1 {
            due.push((DigestKind::Monthly, local.format("%Y-%m").to_string()));
        }

        for (kind, period) in due {
            if !self.claim_run(kind.as_str(), &period).await? {
                continue;
            }
            let (from, to) = cfg.window_utc(kind, now);
            match digest::run_with(&self.collect(kind, from, to).await?, email, false).await {
                Ok(subject) => {
                    tracing::info!("digest: {} за {} отправлен", kind.as_str(), period);
                    sent.push(subject);
                }
                Err(e) => {
                    // Отметку уже поставили — иначе сломанный SMTP будет
                    // ретраить каждые 10 минут до конца дня.
                    tracing::error!("digest: {} за {} не отправлен: {}", kind.as_str(), period, e);
                }
            }
        }

        Ok(sent)
    }
}
