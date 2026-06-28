use chrono::Utc;
use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::{Result, Subscription};

use super::pg::PgClientManager;

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

        let row = client
            .query_one(
                r#"
            INSERT INTO subscriptions
            (id, expires_at, referred_by, refer_code, referral_bonus_awarded)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
                &[
                    &new_sub.id,
                    &new_sub.expires_at,
                    &new_sub.referred_by,
                    &ref_code,
                    &new_sub.referral_bonus_awarded,
                ],
            )
            .await?;

        Ok(Subscription::from(row))
    }

    pub async fn update_subscription(
        &self,
        id: uuid::Uuid,
        expires_at: chrono::DateTime<chrono::Utc>,
        referred_by: Option<&str>,
        ref_code: &String,
    ) -> Result<Subscription> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let now = chrono::Utc::now();

        let row = client
            .query_one(
                r#"
            UPDATE subscriptions
            SET expires_at  = $1,
                referred_by = $2,
                updated_at  = $3,
                refer_code = $4
            WHERE id = $5
            RETURNING *
            "#,
                &[&expires_at, &referred_by, &now, &ref_code, &id],
            )
            .await?;

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
}
