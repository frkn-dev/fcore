use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::Result;

use super::pg::PgClientManager;

#[derive(Debug, Clone)]
pub struct NodeAccessTokenRow {
    pub token: String,
    pub node_uuid: uuid::Uuid,
}

pub struct PgNodeAccessToken {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgNodeAccessToken {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn ensure_table(&self) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS node_access_tokens (
                    token TEXT PRIMARY KEY,
                    node_uuid UUID NOT NULL UNIQUE,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
                );
                "#,
            )
            .await?;
        Ok(())
    }

    pub async fn all(&self) -> Result<Vec<NodeAccessTokenRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client
            .query(
                "SELECT token, node_uuid FROM node_access_tokens",
                &[],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| NodeAccessTokenRow {
                token: r.get("token"),
                node_uuid: r.get("node_uuid"),
            })
            .collect())
    }

    pub async fn upsert(&self, token: &str, node_uuid: uuid::Uuid) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .execute(
                r#"
                INSERT INTO node_access_tokens (token, node_uuid)
                VALUES ($1, $2)
                ON CONFLICT (node_uuid) DO UPDATE SET token = EXCLUDED.token
                "#,
                &[&token, &node_uuid],
            )
            .await?;
        Ok(())
    }

    pub async fn find_by_token(&self, token: &str) -> Result<Option<uuid::Uuid>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let row = client
            .query_opt(
                "SELECT node_uuid FROM node_access_tokens WHERE token = $1",
                &[&token],
            )
            .await?;
        Ok(row.map(|r| r.get(0)))
    }
}
