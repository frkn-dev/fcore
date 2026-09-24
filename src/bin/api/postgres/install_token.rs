use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::{Env, Result};

use super::pg::PgClientManager;

pub const INSTALL_TOKEN_TTL_SECS: i64 = 3600;
pub const MAX_ACTIVE_INSTALL_TOKENS: i64 = 3;
pub const MAX_PRIVATE_NODES_PER_SUB: i64 = 3;

#[derive(Debug, Clone)]
pub struct InstallTokenRow {
    pub token: String,
    pub subscription_id: uuid::Uuid,
    pub scope_env: Env,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

pub struct PgInstallToken {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgInstallToken {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn ensure_table(&self) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS install_tokens (
                    token TEXT PRIMARY KEY,
                    subscription_id UUID NOT NULL,
                    scope_env TEXT NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                    expires_at TIMESTAMPTZ NOT NULL,
                    used_at TIMESTAMPTZ,
                    revoked_at TIMESTAMPTZ
                );

                CREATE INDEX IF NOT EXISTS install_tokens_sub_active_idx
                    ON install_tokens (subscription_id)
                    WHERE used_at IS NULL AND revoked_at IS NULL;
                "#,
            )
            .await?;

        Ok(())
    }

    fn map_row(row: tokio_postgres::Row) -> InstallTokenRow {
        let scope_env: String = row.get("scope_env");
        InstallTokenRow {
            token: row.get("token"),
            subscription_id: row.get("subscription_id"),
            scope_env: Env::from(scope_env.as_str()),
            created_at: row.get("created_at"),
            expires_at: row.get("expires_at"),
            used_at: row.get("used_at"),
            revoked_at: row.get("revoked_at"),
        }
    }

    pub async fn count_active(&self, subscription_id: &uuid::Uuid) -> Result<i64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let now = Utc::now();

        let row = client
            .query_one(
                r#"
                SELECT COUNT(*) FROM install_tokens
                WHERE subscription_id = $1
                  AND used_at IS NULL
                  AND revoked_at IS NULL
                  AND expires_at > $2
                "#,
                &[subscription_id, &now],
            )
            .await?;

        Ok(row.get(0))
    }

    pub async fn create(
        &self,
        token: &str,
        subscription_id: uuid::Uuid,
        scope_env: &Env,
    ) -> Result<InstallTokenRow> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let now = Utc::now();
        let expires_at = now + Duration::seconds(INSTALL_TOKEN_TTL_SECS);
        let scope_env_str = scope_env.to_string();

        let row = client
            .query_one(
                r#"
                INSERT INTO install_tokens
                    (token, subscription_id, scope_env, created_at, expires_at)
                VALUES ($1, $2, $3, $4, $5)
                RETURNING token, subscription_id, scope_env, created_at, expires_at, used_at, revoked_at
                "#,
                &[&token, &subscription_id, &scope_env_str, &now, &expires_at],
            )
            .await?;

        Ok(Self::map_row(row))
    }

    pub async fn find_usable(&self, token: &str) -> Result<Option<InstallTokenRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let now = Utc::now();

        let row = client
            .query_opt(
                r#"
                SELECT token, subscription_id, scope_env, created_at, expires_at, used_at, revoked_at
                FROM install_tokens
                WHERE token = $1
                  AND used_at IS NULL
                  AND revoked_at IS NULL
                  AND expires_at > $2
                "#,
                &[&token, &now],
            )
            .await?;

        Ok(row.map(Self::map_row))
    }

    pub async fn mark_used(&self, token: &str) -> Result<bool> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let now = Utc::now();

        let n = client
            .execute(
                r#"
                UPDATE install_tokens
                SET used_at = $1
                WHERE token = $2
                  AND used_at IS NULL
                  AND revoked_at IS NULL
                  AND expires_at > $1
                "#,
                &[&now, &token],
            )
            .await?;

        Ok(n == 1)
    }
}
