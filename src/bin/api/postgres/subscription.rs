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

    pub async fn create(&self, new_sub: &Subscription) -> Result<Subscription> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let ref_code = new_sub.refer_code.clone();

        let scope_env: Option<String> = new_sub.scope_env.as_ref().map(|e| e.to_string());
        let row = client
            .query_one(
                r#"
            INSERT INTO subscriptions
            (id, expires_at, referred_by, refer_code, referral_bonus_awarded, parent_id, scope_env, premium_token)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING *
            "#,
                &[
                    &new_sub.id,
                    &new_sub.expires_at,
                    &new_sub.referred_by,
                    &ref_code,
                    &new_sub.referral_bonus_awarded,
                    &new_sub.parent_id,
                    &scope_env,
                    &new_sub.premium_token,
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
        referred_by: Option<&str>,
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
                referred_by = $2,
                updated_at  = $3,
                refer_code = $4,
                parent_id = $5,
                scope_env = $6,
                premium_token = $7
            WHERE id = $8
            RETURNING *
            "#,
                &[
                    &expires_at,
                    &referred_by,
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

    pub async fn set_referral_bonus_awarded(
        &self,
        sub_id: &uuid::Uuid,
        awarded: bool,
    ) -> Result<Subscription> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let now = chrono::Utc::now();

        let row = client
            .query_one(
                r#"
                UPDATE subscriptions
                SET referral_bonus_awarded = $1,
                    updated_at = $2
                WHERE id = $3
                RETURNING *
                "#,
                &[&awarded, &now, sub_id],
            )
            .await?;

        Ok(Subscription::from(row))
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
}
