use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::{Client as PgClient, NoTls};
use tracing::{error, warn};

use fcore::Result;

use super::config::PostgresConfig;

pub struct PgManager {
    config: PostgresConfig,
    client: Option<PgClient>,
}

impl PgManager {
    pub async fn new(config: PostgresConfig) -> Result<Self> {
        Ok(Self {
            config,
            client: None,
        })
    }

    async fn connect(&mut self) -> Result<()> {
        let connection_line = format!(
            "host={} user={} dbname={} password={} port={}",
            self.config.host,
            self.config.username,
            self.config.db,
            self.config.password,
            self.config.port
        );

        let (client, connection) = tokio_postgres::connect(&connection_line, NoTls).await?;

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                error!("Postgres connection dropped: {}", e);
            }
        });

        self.client = Some(client);
        Ok(())
    }

    pub async fn get_client(&mut self) -> Result<&mut PgClient> {
        if self.client.is_none() {
            self.connect().await?;
        }

        let client = self.client.as_mut().unwrap();
        if let Err(e) = client.simple_query("SELECT 1").await {
            warn!("PG ping failed: {}. Reconnecting...", e);
            self.connect().await?;
        }

        Ok(self.client.as_mut().unwrap())
    }
}

#[derive(Clone)]
pub struct PgContext {
    manager: Arc<Mutex<PgManager>>,
}

impl PgContext {
    pub async fn init(config: &PostgresConfig) -> Result<Self> {
        let manager = PgManager::new(config.clone()).await?;
        Ok(Self {
            manager: Arc::new(Mutex::new(manager)),
        })
    }

    pub fn emails(&self) -> PgEmails {
        PgEmails::new(self.manager.clone())
    }
}

#[derive(Clone)]
pub struct PgEmails {
    manager: Arc<Mutex<PgManager>>,
}

impl PgEmails {
    pub fn new(manager: Arc<Mutex<PgManager>>) -> Self {
        Self { manager }
    }

    pub async fn insert(
        &self,
        email: Option<&str>,
        email_hmac: Option<&str>,
        trial: bool,
        referred_by: Option<&str>,
        expires_at: Option<DateTime<Utc>>,
        subscription_id: Option<uuid::Uuid>,
        ref_code: Option<&str>,
    ) -> Result<uuid::Uuid> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_one(
                r#"
                INSERT INTO mrkting.emails
                (email, email_hmac, trial, referred_by, expires_at, subscription_id, ref_code)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                RETURNING id
                "#,
                &[
                    &email,
                    &email_hmac,
                    &trial,
                    &referred_by,
                    &expires_at,
                    &subscription_id,
                    &ref_code,
                ],
            )
            .await?;
        Ok(row.get("id"))
    }

    pub async fn find_by_hmac(&self, email_hmac: &str) -> Result<Option<EmailRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_opt(
                "SELECT * FROM mrkting.emails WHERE email_hmac = $1 LIMIT 1",
                &[&email_hmac],
            )
            .await?;
        Ok(row.map(|r| EmailRow::from(r)))
    }

    pub async fn get_by_ref_code(&self, ref_code: &str) -> Result<Option<EmailRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_opt(
                "SELECT * FROM mrkting.emails WHERE ref_code = $1 LIMIT 1",
                &[&ref_code],
            )
            .await?;
        Ok(row.map(|r| EmailRow::from(r)))
    }

    pub async fn count_invited_by(&self, referred_by: &str) -> Result<i64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM mrkting.emails WHERE referred_by = $1 AND subscription_id IS NOT NULL",
                &[&referred_by],
            )
            .await?;
        Ok(row.get::<_, i64>(0))
    }

    pub async fn update_trial_and_expires(
        &self,
        subscription_id: uuid::Uuid,
        expires_at: DateTime<Utc>,
    ) -> Result<u64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client
            .execute(
                "UPDATE mrkting.emails \
                 SET trial = false, expires_at = $1, converted_at = CASE WHEN trial = true THEN now() ELSE converted_at END \
                 WHERE subscription_id = $2",
                &[&expires_at,
                    &subscription_id,
                ],
            )
            .await?;
        Ok(rows)
    }

    pub async fn is_trial(
        &self,
        subscription_id: uuid::Uuid,
    ) -> Result<Option<bool>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_opt(
                "SELECT trial FROM mrkting.emails WHERE subscription_id = $1 LIMIT 1",
                &[&subscription_id,
                ],
            )
            .await?;
        Ok(row.map(|r| r.get("trial")))
    }

    pub async fn trials_by_period(
        &self,
        granularity: &str,
        period: i64,
    ) -> Result<Vec<(String, i64)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let since = match granularity {
            "monthly" => chrono::Utc::now() - chrono::Duration::days(period * 30),
            _ => chrono::Utc::now() - chrono::Duration::days(period),
        };

        let bucket_expr = match granularity {
            "monthly" => "TO_CHAR(DATE_TRUNC('month', created_at), 'YYYY-MM')",
            _ => "TO_CHAR(DATE(created_at), 'YYYY-MM-DD')",
        };

        let sql = format!(
            r#"
            SELECT {bucket_expr} AS bucket, COUNT(*) AS cnt
            FROM mrkting.emails
            WHERE trial = true AND created_at >= $1
            GROUP BY bucket
            ORDER BY bucket DESC
            "#
        );

        let rows = client.query(&sql, &[&since]).await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>("bucket"), r.get::<_, i64>("cnt")))
            .collect())
    }

    pub async fn conversions_by_period(
        &self,
        granularity: &str,
        period: i64,
    ) -> Result<Vec<(String, i64)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let since = match granularity {
            "monthly" => chrono::Utc::now() - chrono::Duration::days(period * 30),
            _ => chrono::Utc::now() - chrono::Duration::days(period),
        };

        let bucket_expr = match granularity {
            "monthly" => "TO_CHAR(DATE_TRUNC('month', converted_at), 'YYYY-MM')",
            _ => "TO_CHAR(DATE(converted_at), 'YYYY-MM-DD')",
        };

        let sql = format!(
            r#"
            SELECT {bucket_expr} AS bucket, COUNT(*) AS cnt
            FROM mrkting.emails
            WHERE converted_at IS NOT NULL AND converted_at >= $1
            GROUP BY bucket
            ORDER BY bucket DESC
            "#
        );

        let rows = client.query(&sql, &[&since]).await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>("bucket"), r.get::<_, i64>("cnt")))
            .collect())
    }
}

#[allow(dead_code)]
pub struct EmailRow {
    pub id: uuid::Uuid,
    pub subscription_id: Option<uuid::Uuid>,
    pub email: Option<String>,
    pub email_hmac: Option<String>,
    pub trial: bool,
    pub referred_by: Option<String>,
    pub ref_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub converted_at: Option<DateTime<Utc>>,
}

impl From<tokio_postgres::Row> for EmailRow {
    fn from(row: tokio_postgres::Row) -> Self {
        Self {
            id: row.get("id"),
            subscription_id: row.get("subscription_id"),
            email: row.get("email"),
            email_hmac: row.get("email_hmac"),
            trial: row.get("trial"),
            referred_by: row.get("referred_by"),
            ref_code: row.get("ref_code"),
            created_at: row.get("created_at"),
            expires_at: row.get("expires_at"),
            converted_at: row.get("converted_at"),
        }
    }
}
