use chrono::Utc;
use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::{Env, Result, Subscription};

use super::{super::subscription_audit, pg::PgClientManager};

pub struct PgSubscription {
    pub manager: Arc<Mutex<PgClientManager>>,
}

/// Result of a limit-aware selfhosted-user insert
/// (`PgSubscription::insert_child_within_user_limit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserSlotOutcome {
    /// `false` when the account already sits at its user cap: nothing was
    /// inserted.
    pub inserted: bool,
    /// Live child subscriptions of the account after the call.
    pub used: i64,
    /// The account's user cap at the time of the check.
    pub limit: i64,
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

    /// One `INSERT INTO subscriptions` for a freshly built row, shared by
    /// `create` and the limit-aware child insert so both always write the
    /// same set of columns. Generic over the client so it works both on a
    /// pooled connection and inside a transaction.
    async fn insert_row<T>(client: &T, new_sub: &Subscription) -> Result<Subscription>
    where
        T: tokio_postgres::GenericClient + Sync,
    {
        let scope_env: Option<String> = new_sub.scope_env.as_ref().map(|e| e.to_string());
        let plan_kind = new_sub.plan_kind.to_string();
        let product = new_sub.product.to_string();
        let server_limit = new_sub.server_limit as i32;
        let user_limit = new_sub.user_limit as i32;
        let row = client
            .query_one(
                r#"
            INSERT INTO subscriptions
            (id, expires_at, refer_code, parent_id, scope_env, premium_token, plan_kind, limit_bytes, server_limit, user_limit, product)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING *
            "#,
                &[
                    &new_sub.id,
                    &new_sub.expires_at,
                    &new_sub.refer_code,
                    &new_sub.parent_id,
                    &scope_env,
                    &new_sub.premium_token,
                    &plan_kind,
                    &new_sub.limit_bytes,
                    &server_limit,
                    &user_limit,
                    &product,
                ],
            )
            .await?;

        Ok(Subscription::from(row))
    }

    pub async fn create(&self, new_sub: &Subscription) -> Result<Subscription> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let created = Self::insert_row(&*client, new_sub).await?;

        subscription_audit::log_days_change(
            "db_created",
            new_sub.id,
            None,
            new_sub.expires_at,
            None,
            "PgSubscription::create",
        );

        Ok(created)
    }

    /// Limit-aware insert of a selfhosted "user" (child subscription): locks
    /// the account row, counts its live children and inserts the child in one
    /// transaction, so two concurrent invites cannot both take the last free
    /// slot. `Ok(None)` means no live account with that id; the returned
    /// outcome with `inserted == false` means the user cap is already full.
    pub async fn insert_child_within_user_limit(
        &self,
        owner_id: &uuid::Uuid,
        child: &Subscription,
    ) -> Result<Option<UserSlotOutcome>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let tx = client.transaction().await?;

        let Some(row) = tx
            .query_opt(
                "SELECT user_limit FROM subscriptions
                 WHERE id = $1 AND is_deleted = false
                 FOR UPDATE",
                &[owner_id],
            )
            .await?
        else {
            return Ok(None);
        };
        let user_limit: i32 = row
            .try_get::<_, i32>("user_limit")
            .unwrap_or(fcore::DEFAULT_USER_LIMIT as i32)
            .max(0);

        // A deleted child frees its slot; lowering the cap never deletes
        // anything, it only stops new users.
        let used: i64 = tx
            .query_one(
                "SELECT count(*) FROM subscriptions
                 WHERE parent_id = $1 AND is_deleted = false",
                &[owner_id],
            )
            .await?
            .get(0);

        if used >= user_limit as i64 {
            // Dropping the transaction rolls back the (read-only) work.
            return Ok(Some(UserSlotOutcome {
                inserted: false,
                used,
                limit: user_limit as i64,
            }));
        }

        Self::insert_row(&tx, child).await?;
        tx.commit().await?;
        Ok(Some(UserSlotOutcome {
            inserted: true,
            used: used + 1,
            limit: user_limit as i64,
        }))
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

    /// Admin-only: replace the selfhosted caps of an account. A lower limit
    /// deletes nothing — existing nodes and child subscriptions are left
    /// alone, the account just can't grow past the new cap.
    pub async fn set_selfhosted_limits(
        &self,
        sub_id: &uuid::Uuid,
        server_limit: i64,
        user_limit: i64,
    ) -> Result<Subscription> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let now = chrono::Utc::now();
        let server_limit = server_limit as i32;
        let user_limit = user_limit as i32;

        let row = client
            .query_one(
                r#"
                UPDATE subscriptions
                SET server_limit = $1,
                    user_limit = $2,
                    updated_at = $3
                WHERE id = $4
                RETURNING *
                "#,
                &[&server_limit, &user_limit, &now, sub_id],
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
