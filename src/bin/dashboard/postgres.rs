use bcrypt::{hash, verify, DEFAULT_COST};
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::{Client as PgClient, NoTls};
use tracing::{error, warn};
use uuid::Uuid;

use crate::config::PostgresConfig;

pub type Result<T> = std::result::Result<T, tokio_postgres::Error>;

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
    pub async fn init(config: &PostgresConfig) -> anyhow::Result<Self> {
        let manager = PgManager::new(config.clone()).await?;
        Ok(Self {
            manager: Arc::new(Mutex::new(manager)),
        })
    }

    pub fn partners(&self) -> PgPartners {
        PgPartners::new(self.manager.clone())
    }

    pub fn sessions(&self) -> PgSessions {
        PgSessions::new(self.manager.clone())
    }

    pub fn promocodes(&self) -> PgPartnerPromocodes {
        PgPartnerPromocodes::new(self.manager.clone())
    }
}

#[derive(Clone)]
pub struct PgPartners {
    manager: Arc<Mutex<PgManager>>,
}

impl PgPartners {
    pub fn new(manager: Arc<Mutex<PgManager>>) -> Self {
        Self { manager }
    }

    pub async fn create(
        &self,
        email: &str,
        password: &str,
        name: &str,
        share_percent: f64,
        show_share: bool,
    ) -> anyhow::Result<Uuid> {
        let password_hash = hash(password, DEFAULT_COST)?;
        let share_str = share_percent.to_string();
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_one(
                r#"
                INSERT INTO dashboard.partners
                (email, password_hash, name, share_percent, show_share)
                VALUES ($1, $2, $3, $4::numeric, $5)
                RETURNING id
                "#,
                &[&email,&password_hash,&name,&share_str,&show_share,
                ],
            )
            .await?;
        Ok(row.get("id"))
    }

    pub async fn find_by_email(&self, email: &str) -> anyhow::Result<Option<PartnerRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_opt(
                "SELECT * FROM dashboard.partners WHERE email = $1 LIMIT 1",
                &[&email],
            )
            .await?;
        Ok(row.map(|r| PartnerRow::from(r)))
    }

    pub async fn find_by_id(&self, id: Uuid) -> anyhow::Result<Option<PartnerRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_opt(
                "SELECT * FROM dashboard.partners WHERE id = $1 LIMIT 1",
                &[&id],
            )
            .await?;
        Ok(row.map(|r| PartnerRow::from(r)))
    }

    pub async fn verify_password(
        &self,
        email: &str,
        password: &str,
    ) -> anyhow::Result<Option<PartnerRow>> {
        if let Some(partner) = self.find_by_email(email).await? {
            if verify(password, &partner.password_hash)? {
                return Ok(Some(partner));
            }
        }
        Ok(None)
    }
}

#[allow(dead_code)]
pub struct PartnerRow {
    pub id: Uuid,
    pub email: String,
    pub password_hash: String,
    pub name: String,
    pub share_percent: f64,
    pub show_share: bool,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

impl From<tokio_postgres::Row> for PartnerRow {
    fn from(row: tokio_postgres::Row) -> Self {
        Self {
            id: row.get("id"),
            email: row.get("email"),
            password_hash: row.get("password_hash"),
            name: row.get("name"),
            share_percent: row.get("share_percent"),
            show_share: row.get("show_share"),
            active: row.get("active"),
            created_at: row.get("created_at"),
        }
    }
}

#[derive(Clone)]
pub struct PgSessions {
    manager: Arc<Mutex<PgManager>>,
}

impl PgSessions {
    pub fn new(manager: Arc<Mutex<PgManager>>) -> Self {
        Self { manager }
    }

    pub async fn create(
        &self,
        partner_id: Uuid,
        ttl_hours: i64,
    ) -> anyhow::Result<String> {
        let token = Uuid::new_v4().to_string();
        let expires_at = Utc::now() + Duration::hours(ttl_hours);
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .execute(
                r#"
                INSERT INTO dashboard.partner_sessions
                (partner_id, token, expires_at)
                VALUES ($1, $2, $3)
                "#,
                &[&partner_id, &token, &expires_at],
            )
            .await?;
        Ok(token)
    }

    pub async fn find_by_token(
        &self,
        token: &str,
    ) -> anyhow::Result<Option<PartnerSessionRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_opt(
                "SELECT * FROM dashboard.partner_sessions WHERE token = $1 AND expires_at > now() LIMIT 1",
                &[&token],
            )
            .await?;
        Ok(row.map(|r| PartnerSessionRow::from(r)))
    }

    #[allow(dead_code)]
    pub async fn delete(&self, token: &str) -> anyhow::Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .execute(
                "DELETE FROM dashboard.partner_sessions WHERE token = $1",
                &[&token],
            )
            .await?;
        Ok(())
    }
}

#[allow(dead_code)]
pub struct PartnerSessionRow {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl From<tokio_postgres::Row> for PartnerSessionRow {
    fn from(row: tokio_postgres::Row) -> Self {
        Self {
            id: row.get("id"),
            partner_id: row.get("partner_id"),
            token: row.get("token"),
            expires_at: row.get("expires_at"),
            created_at: row.get("created_at"),
        }
    }
}

#[derive(Clone)]
pub struct PgPartnerPromocodes {
    manager: Arc<Mutex<PgManager>>,
}

impl PgPartnerPromocodes {
    pub fn new(manager: Arc<Mutex<PgManager>>) -> Self {
        Self { manager }
    }

    pub async fn create(
        &self,
        partner_id: Uuid,
        code: &str,
        discount_percent: i32,
        max_uses: Option<i32>,
        duration_days: Option<i32>,
        expires_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Uuid> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_one(
                r#"
                INSERT INTO dashboard.partner_promocodes
                (partner_id, code, discount_percent, max_uses, duration_days, expires_at)
                VALUES ($1, $2, $3, $4, $5, $6)
                RETURNING id
                "#,
                &[
                    &partner_id,
                    &code,
                    &discount_percent,
                    &max_uses,
                    &duration_days,
                    &expires_at,
                ],
            )
            .await?;
        Ok(row.get("id"))
    }

    pub async fn list(&self, partner_id: Uuid) -> anyhow::Result<Vec<PartnerPromocodeRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client
            .query(
                "SELECT * FROM dashboard.partner_promocodes WHERE partner_id = $1 ORDER BY created_at DESC",
                &[&partner_id],
            )
            .await?;
        Ok(rows.into_iter().map(|r| PartnerPromocodeRow::from(r)).collect())
    }

    pub async fn attach(
        &self,
        partner_id: Uuid,
        code: &str,
        payment_promocode_id: Uuid,
        discount_percent: i32,
        max_uses: Option<i32>,
        duration_days: Option<i32>,
        expires_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Uuid> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_one(
                r#"
                INSERT INTO dashboard.partner_promocodes
                (partner_id, code, payment_promocode_id, discount_percent, max_uses, duration_days, expires_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                RETURNING id
                "#,
                &[
                    &partner_id,
                    &code,
                    &payment_promocode_id,
                    &discount_percent,
                    &max_uses,
                    &duration_days,
                    &expires_at,
                ],
            )
            .await?;
        Ok(row.get("id"))
    }

    pub async fn set_payment_id(
        &self,
        id: Uuid,
        partner_id: Uuid,
        payment_promocode_id: Uuid,
    ) -> anyhow::Result<u64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client
            .execute(
                "UPDATE dashboard.partner_promocodes SET payment_promocode_id = $1 WHERE id = $2 AND partner_id = $3",
                &[&payment_promocode_id, &id, &partner_id],
            )
            .await?;
        Ok(rows)
    }

    pub async fn find_by_id(
        &self,
        id: Uuid,
        partner_id: Uuid,
    ) -> anyhow::Result<Option<PartnerPromocodeRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_opt(
                "SELECT * FROM dashboard.partner_promocodes WHERE id = $1 AND partner_id = $2 LIMIT 1",
                &[&id, &partner_id],
            )
            .await?;
        Ok(row.map(|r| PartnerPromocodeRow::from(r)))
    }

    pub async fn delete(&self, id: Uuid, partner_id: Uuid) -> anyhow::Result<u64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client
            .execute(
                "DELETE FROM dashboard.partner_promocodes WHERE id = $1 AND partner_id = $2",
                &[&id, &partner_id],
            )
            .await?;
        Ok(rows)
    }
}

#[allow(dead_code)]
pub struct PartnerPromocodeRow {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub code: String,
    pub payment_promocode_id: Option<Uuid>,
    pub discount_percent: i32,
    pub max_uses: Option<i32>,
    pub duration_days: Option<i32>,
    pub expires_at: Option<DateTime<Utc>>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

impl From<tokio_postgres::Row> for PartnerPromocodeRow {
    fn from(row: tokio_postgres::Row) -> Self {
        Self {
            id: row.get("id"),
            partner_id: row.get("partner_id"),
            code: row.get("code"),
            payment_promocode_id: row.get("payment_promocode_id"),
            discount_percent: row.get("discount_percent"),
            max_uses: row.get("max_uses"),
            duration_days: row.get("duration_days"),
            expires_at: row.get("expires_at"),
            active: row.get("active"),
            created_at: row.get("created_at"),
        }
    }
}
