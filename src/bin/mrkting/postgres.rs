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

    pub fn surveys(&self) -> PgSurveys {
        PgSurveys::new(self.manager.clone())
    }

    pub fn survey_campaigns(&self) -> PgSurveyCampaigns {
        PgSurveyCampaigns::new(self.manager.clone())
    }

    pub fn survey_keys(&self) -> PgSurveyKeys {
        PgSurveyKeys::new(self.manager.clone())
    }
}

#[derive(Clone)]
pub struct PgSurveys {
    manager: Arc<Mutex<PgManager>>,
}

impl PgSurveys {
    pub fn new(manager: Arc<Mutex<PgManager>>) -> Self {
        Self { manager }
    }

    pub async fn find_by_hmac_and_campaign(
        &self,
        email_hmac: &str,
        campaign_id: uuid::Uuid,
    ) -> Result<Option<SurveyRewardRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_opt(
                "SELECT * FROM mrkting.survey_rewards WHERE email_hmac = $1 AND campaign_id = $2 LIMIT 1",
                &[&email_hmac, &campaign_id],
            )
            .await?;
        Ok(row.map(|r| SurveyRewardRow::from(r)))
    }

    pub async fn insert(
        &self,
        email: &str,
        email_hmac: &str,
        campaign_id: uuid::Uuid,
        key_id: uuid::Uuid,
    ) -> Result<uuid::Uuid> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_one(
                r#"
                INSERT INTO mrkting.survey_rewards
                (email, email_hmac, campaign_id, key_id, rewarded_at)
                VALUES ($1, $2, $3, $4, now())
                RETURNING id
                "#,
                &[&email, &email_hmac, &campaign_id, &key_id],
            )
            .await?;
        Ok(row.get("id"))
    }
}

#[allow(dead_code)]
pub struct SurveyRewardRow {
    pub id: uuid::Uuid,
    pub email: String,
    pub email_hmac: String,
    pub campaign_id: Option<uuid::Uuid>,
    pub answers: Option<serde_json::Value>,
    pub key_id: Option<uuid::Uuid>,
    pub rewarded_at: DateTime<Utc>,
}

impl From<tokio_postgres::Row> for SurveyRewardRow {
    fn from(row: tokio_postgres::Row) -> Self {
        Self {
            id: row.get("id"),
            email: row.get("email"),
            email_hmac: row.get("email_hmac"),
            campaign_id: row.get("campaign_id"),
            answers: row.get("answers"),
            key_id: row.get("key_id"),
            rewarded_at: row.get("rewarded_at"),
        }
    }
}

#[derive(Clone)]
pub struct PgSurveyCampaigns {
    manager: Arc<Mutex<PgManager>>,
}

impl PgSurveyCampaigns {
    pub fn new(manager: Arc<Mutex<PgManager>>) -> Self {
        Self { manager }
    }

    pub async fn find_by_name(
        &self,
        name: &str,
    ) -> Result<Option<SurveyCampaignRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_opt(
                "SELECT * FROM mrkting.survey_campaigns WHERE name = $1 LIMIT 1",
                &[&name],
            )
            .await?;
        Ok(row.map(|r| SurveyCampaignRow::from(r)))
    }

    pub async fn find_by_token(
        &self,
        token: &str,
    ) -> Result<Option<SurveyCampaignRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_opt(
                "SELECT * FROM mrkting.survey_campaigns WHERE token = $1 LIMIT 1",
                &[&token],
            )
            .await?;
        Ok(row.map(|r| SurveyCampaignRow::from(r)))
    }

    pub async fn insert(
        &self,
        name: &str,
        token: &str,
        distributor: &str,
        key_days: i32,
        campaign_days: i32,
        limit_bytes: Option<i64>,
        subject: Option<&str>,
        utm_campaign: Option<&str>,
        starts_at: DateTime<Utc>,
    ) -> Result<uuid::Uuid> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_one(
                r#"
                INSERT INTO mrkting.survey_campaigns
                (name, token, distributor, key_days, campaign_days, limit_bytes, subject, utm_campaign, starts_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                RETURNING id
                "#,
                &[
                    &name,
                    &token,
                    &distributor,
                    &key_days,
                    &campaign_days,
                    &limit_bytes,
                    &subject,
                    &utm_campaign,
                    &starts_at,
                ],
            )
            .await?;
        Ok(row.get("id"))
    }

    #[allow(dead_code)]
    pub async fn list(&self) -> Result<Vec<SurveyCampaignRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client
            .query(
                "SELECT * FROM mrkting.survey_campaigns ORDER BY created_at DESC",
                &[],
            )
            .await?;
        Ok(rows.into_iter().map(|r| SurveyCampaignRow::from(r)).collect())
    }
}

#[allow(dead_code)]
pub struct SurveyCampaignRow {
    pub id: uuid::Uuid,
    pub name: String,
    pub token: String,
    pub distributor: String,
    pub key_days: i32,
    pub campaign_days: i32,
    pub limit_bytes: Option<i64>,
    pub subject: Option<String>,
    pub utm_campaign: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

impl From<tokio_postgres::Row> for SurveyCampaignRow {
    fn from(row: tokio_postgres::Row) -> Self {
        Self {
            id: row.get("id"),
            name: row.get("name"),
            token: row.get("token"),
            distributor: row.get("distributor"),
            key_days: row.get("key_days"),
            campaign_days: row.get("campaign_days"),
            limit_bytes: row.get("limit_bytes"),
            subject: row.get("subject"),
            utm_campaign: row.get("utm_campaign"),
            starts_at: row.get("starts_at"),
            active: row.get("active"),
            created_at: row.get("created_at"),
        }
    }
}

#[derive(Clone)]
pub struct PgSurveyKeys {
    manager: Arc<Mutex<PgManager>>,
}

impl PgSurveyKeys {
    pub fn new(manager: Arc<Mutex<PgManager>>) -> Self {
        Self { manager }
    }

    pub async fn insert_keys(
        &self,
        campaign_id: uuid::Uuid,
        keys: &[(uuid::Uuid, String)],
    ) -> Result<u64> {
        if keys.is_empty() {
            return Ok(0);
        }
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let mut stmt = String::from(
            "INSERT INTO mrkting.survey_keys (campaign_id, key_id, code) VALUES "
        );
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
        for (i, (key_id, code)) in keys.iter().enumerate() {
            if i > 0 {
                stmt.push_str(", ");
            }
            let offset = i * 3;
            stmt.push_str(&format!(
                "(${}, ${}, ${})",
                offset + 1,
                offset + 2,
                offset + 3
            ));
            params.push(&campaign_id);
            params.push(key_id);
            params.push(code);
        }
        stmt.push_str(" ON CONFLICT (campaign_id, code) DO NOTHING");

        let rows = client.execute(&stmt, &params).await?;
        Ok(rows)
    }

    pub async fn take_key(
        &self,
        campaign_id: uuid::Uuid,
        email_hmac: &str,
    ) -> Result<Option<SurveyKeyRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let tx = client.transaction().await?;
        let row = tx
            .query_opt(
                r#"
                UPDATE mrkting.survey_keys
                SET status = 'issued', email_hmac = $2, issued_at = now()
                WHERE id = (
                    SELECT id FROM mrkting.survey_keys
                    WHERE campaign_id = $1 AND status = 'available'
                    ORDER BY created_at ASC
                    FOR UPDATE SKIP LOCKED
                    LIMIT 1
                )
                RETURNING *
                "#,
                &[&campaign_id, &email_hmac],
            )
            .await?;
        tx.commit().await?;

        Ok(row.map(|r| SurveyKeyRow::from(r)))
    }

    #[allow(dead_code)]
    pub async fn count_available(
        &self,
        campaign_id: uuid::Uuid,
    ) -> Result<i64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM mrkting.survey_keys WHERE campaign_id = $1 AND status = 'available'",
                &[&campaign_id],
            )
            .await?;
        let count: i64 = row.get(0);
        Ok(count)
    }
}

#[allow(dead_code)]
pub struct SurveyKeyRow {
    pub id: uuid::Uuid,
    pub campaign_id: uuid::Uuid,
    pub key_id: uuid::Uuid,
    pub code: String,
    pub status: String,
    pub email_hmac: Option<String>,
    pub issued_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<tokio_postgres::Row> for SurveyKeyRow {
    fn from(row: tokio_postgres::Row) -> Self {
        Self {
            id: row.get("id"),
            campaign_id: row.get("campaign_id"),
            key_id: row.get("key_id"),
            code: row.get("code"),
            status: row.get("status"),
            email_hmac: row.get("email_hmac"),
            issued_at: row.get("issued_at"),
            created_at: row.get("created_at"),
        }
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

    pub async fn referrals_by_period(
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
            WHERE referred_by IS NOT NULL AND referred_by <> 'WEB' AND created_at >= $1
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
