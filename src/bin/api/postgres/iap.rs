use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::Result;

use super::pg::PgClientManager;

/// Persists the App Store original_transaction_id -> subscription_id binding.
///
/// Stored in a dedicated table (not in `subscriptions`) so the shared
/// `Subscription` model stays untouched; survives service restarts.
pub struct PgIap {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgIap {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn ensure_table(&self) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS iap_transactions (
                    original_transaction_id TEXT PRIMARY KEY,
                    subscription_id UUID NOT NULL,
                    product_id TEXT,
                    environment TEXT,
                    installation_uuid UUID,
                    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT now(),
                    updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT now()
                )
                "#,
            )
            .await?;

        Ok(())
    }

    /// Atomically binds `original_transaction_id` to `new_subscription_id`.
    ///
    /// Returns the bound subscription id: `new_subscription_id` when the binding
    /// was created by this call, or the previously bound id when the transaction
    /// was already known (idempotency for repeated/restore calls).
    pub async fn bind_or_get(
        &self,
        original_transaction_id: &str,
        new_subscription_id: &uuid::Uuid,
        product_id: &str,
        environment: &str,
        installation_uuid: Option<uuid::Uuid>,
    ) -> Result<uuid::Uuid> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let inserted = client
            .query_opt(
                r#"
                INSERT INTO iap_transactions
                (original_transaction_id, subscription_id, product_id, environment, installation_uuid)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (original_transaction_id) DO NOTHING
                RETURNING subscription_id
                "#,
                &[
                    &original_transaction_id,
                    new_subscription_id,
                    &product_id,
                    &environment,
                    &installation_uuid,
                ],
            )
            .await?;

        if let Some(row) = inserted {
            return Ok(row.get(0));
        }

        let row = client
            .query_one(
                "SELECT subscription_id FROM iap_transactions WHERE original_transaction_id = $1",
                &[&original_transaction_id],
            )
            .await?;

        Ok(row.get(0))
    }
}
