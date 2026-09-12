use chrono::DateTime;
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::pg::PgClientManager;

use fcore::{Key, Result};

pub struct PgKey {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgKey {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn get(&self, code: &str) -> Option<Key> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await.ok()?;

        let query = "
            SELECT id, code, activated, created_at, modified_at, subscription_id, days, distributor, kind, traffic_bytes
            FROM keys
            WHERE code = $1
        ";

        let rows = client.query(query, &[&code]).await.ok()?;

        rows.first().cloned().map(|r| r.into())
    }

    pub async fn insert(&self, key: &Key) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let query = "
               INSERT INTO keys (id, code, activated, created_at, modified_at, subscription_id, days, distributor, kind, traffic_bytes)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
           ";

        client
            .execute(
                query,
                &[
                    &key.id,
                    &key.code,
                    &key.activated,
                    &key.created_at,
                    &key.modified_at,
                    &key.subscription_id,
                    &key.days,
                    &key.distributor.as_str(),
                    &key.kind.to_string(),
                    &key.traffic_bytes,
                ],
            )
            .await?;

        Ok(())
    }

    pub async fn activate(&self, key: &Key) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let tx = client.transaction().await?;

        let modified_at: DateTime<Utc> = Utc::now();
        tx.execute(
            "
            UPDATE keys
            SET activated = true,
                subscription_id = $1,
                modified_at = $2
            WHERE id = $3
            ",
            &[&key.subscription_id, &modified_at, &key.id],
        )
        .await?;

        tx.commit().await?;
        Ok(())
    }
}
