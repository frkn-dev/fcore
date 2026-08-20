use chrono::Utc;
use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::{Env, Result, Subscription};

use super::{
    super::subscription_audit,
    pg::PgClientManager,
};

pub struct PgSubscription {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgSubscription {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn all(&self) -> Result<Vec<Subscription>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                "SELECT * FROM subscriptions WHERE NOT is_deleted ORDER BY created_at DESC",
                &[],
            )
            .await?;

        let subscriptions: Vec<Subscription> = rows.into_iter().map(Subscription::from).collect();

        Ok(subscriptions)
    }

    pub async fn find(&self, id: &uuid::Uuid) -> Result<Option<Subscription>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let row = client
            .query_opt(
                "SELECT * FROM subscriptions WHERE id = $1 AND NOT is_deleted",
                &[id],
            )
            .await?;

        Ok(row.map(Subscription::from))
    }

    pub async fn create(&self, new_sub: &Subscription) -> Result<Subscription> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let ref_code = new_sub.refer_code.clone();

        let scope_env: Option<String> = new_sub.scope_env.as_ref().map(|e| e.to_string());
        let plan_kind = new_sub.plan_kind.to_string();
        let row = client
            .query_one(
                r#"
            INSERT INTO subscriptions
            (id, expires_at, refer_code, parent_id, scope_env, premium_token, plan_kind, limit_bytes)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING *
            "#,
                &[
                    &new_sub.id,
                    &new_sub.expires_at,
                    &ref_code,
                    &new_sub.parent_id,
                    &scope_env,
                    &new_sub.premium_token,
                    &plan_kind,
                    &new_sub.limit_bytes,
                ],
            )
            .await?;

        subscription_audit::log_days_change(
            "db_created",
            new_sub.id,
            None,
            new_sub.expires_at,
            None,
            "PgSubscription::create",
        );

        Ok(Subscription::from(row))
    }

    pub async fn update_subscription(
        &self,
        id: uuid::Uuid,
        expires_at: chrono::DateTime<chrono::Utc>,
        ref_code: &String,
        parent_id: Option<uuid::Uuid>,
        scope_env: Option<&Env>,
        premium_token: Option<&str>,
    ) -> Result<Subscription> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let now = chrono::Utc::now();

        let scope_env_str: Option<String> = scope_env.map(|e| e.to_string());

        let row = client
            .query_one(
                r#"
            UPDATE subscriptions
            SET expires_at  = $1,
                updated_at  = $2,
                refer_code = $3,
                parent_id = $4,
                scope_env = $5,
                premium_token = $6
            WHERE id = $7
            RETURNING *
            "#,
                &[
                    &expires_at,
                    &now,
                    ref_code,
                    &parent_id,
                    &scope_env_str,
                    &premium_token,
                    &id,
                ],
            )
            .await?;

        subscription_audit::log_days_change(
            "db_updated",
            id,
            None,
            Some(expires_at),
            None,
            "PgSubscription::update_subscription",
        );

        Ok(Subscription::from(row))
    }

    pub async fn add_days(&self, sub_id: &uuid::Uuid, days: i64) -> Result<Subscription> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let now = chrono::Utc::now();

        let row = client
            .query_one(
                "SELECT expires_at FROM subscriptions WHERE id = $1",
                &[sub_id],
            )
            .await?;

        let current_expires_at: Option<chrono::DateTime<Utc>> = row.get("expires_at");

        let base = match current_expires_at {
            Some(exp) if exp > now => exp,
            _ => now,
        };

        let new_expires_at = base + chrono::Duration::days(days);

        let updated_row = client
            .query_one(
                r#"
                UPDATE subscriptions
                SET expires_at = $1,
                    updated_at = $2
                WHERE id = $3
                RETURNING *
                "#,
                &[&new_expires_at, &now, sub_id],
            )
            .await?;

        subscription_audit::log_days_change(
            "db_days_added",
            *sub_id,
            current_expires_at,
            Some(new_expires_at),
            Some(days),
            "PgSubscription::add_days",
        );

        Ok(Subscription::from(updated_row))
    }

    /// Atomically records a traffic top-up and increments limit_bytes.
    /// Idempotent by trace_id: Ok(None) means this trace_id was already
    /// applied (no double-add), Ok(Some(limit)) is the new limit.
    pub async fn add_limit_bytes(
        &self,
        sub_id: &uuid::Uuid,
        trace_id: &uuid::Uuid,
        bytes: i64,
    ) -> Result<Option<i64>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let tx = client.transaction().await?;

        let recorded = tx
            .query_opt(
                r#"
                INSERT INTO traffic_topups (trace_id, subscription_id, bytes)
                VALUES ($1, $2, $3)
                ON CONFLICT (trace_id) DO NOTHING
                RETURNING trace_id
                "#,
                &[trace_id, sub_id, &bytes],
            )
            .await?;

        let new_limit = match recorded {
            None => None,
            Some(_) => {
                let row = tx
                    .query_one(
                        r#"
                        UPDATE subscriptions
                        SET limit_bytes = COALESCE(limit_bytes, 0) + $1,
                            updated_at = $2
                        WHERE id = $3
                        RETURNING limit_bytes
                        "#,
                        &[&bytes, &chrono::Utc::now(), sub_id],
                    )
                    .await?;
                Some(row.get::<_, i64>("limit_bytes"))
            }
        };

        tx.commit().await?;
        Ok(new_limit)
    }

    pub async fn set_premium_fields(
        &self,
        sub_id: &uuid::Uuid,
        scope_env: Option<&Env>,
        premium_token: Option<&str>,
    ) -> Result<Subscription> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let now = chrono::Utc::now();
        let scope_env_str: Option<String> = scope_env.map(|e| e.to_string());

        let row = client
            .query_one(
                r#"
                UPDATE subscriptions
                SET scope_env = $1,
                    premium_token = $2,
                    updated_at = $3
                WHERE id = $4
                RETURNING *
                "#,
                &[&scope_env_str, &premium_token, &now, sub_id],
            )
            .await?;

        Ok(Subscription::from(row))
    }

    pub async fn delete(&self, sub_id: &uuid::Uuid) -> Result<Subscription> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let now = chrono::Utc::now();

        let row = client
            .query_one(
                r#"
                UPDATE subscriptions
                SET is_deleted = true,
                    updated_at = $1
                WHERE id = $2
                RETURNING *
                "#,
                &[&now, sub_id],
            )
            .await?;

        Ok(Subscription::from(row))
    }
}
