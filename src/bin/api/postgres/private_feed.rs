use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::Result;

use super::pg::PgClientManager;

pub struct PgPrivateFeed {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgPrivateFeed {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn ensure_table(&self) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS private_node_feed (
                    node_uuid UUID PRIMARY KEY,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
                );
                "#,
            )
            .await?;
        Ok(())
    }

    pub async fn all(&self) -> Result<Vec<uuid::Uuid>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client
            .query("SELECT node_uuid FROM private_node_feed", &[])
            .await?;
        Ok(rows.into_iter().map(|row| row.get("node_uuid")).collect())
    }

    pub async fn insert(&self, node_uuid: uuid::Uuid) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .execute(
                "INSERT INTO private_node_feed (node_uuid) VALUES ($1) ON CONFLICT DO NOTHING",
                &[&node_uuid],
            )
            .await?;
        Ok(())
    }

    pub async fn delete(&self, node_uuid: uuid::Uuid) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .execute(
                "DELETE FROM private_node_feed WHERE node_uuid = $1",
                &[&node_uuid],
            )
            .await?;
        Ok(())
    }
}
