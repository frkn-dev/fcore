use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_postgres::Client;
use uuid::Uuid;

pub const SUBSCRIPTION_SQL: &str = "SELECT is_deleted FROM subscriptions WHERE id = $1";

pub fn grants_mesh(is_deleted: bool, expires_at_unix: Option<i64>, now: i64) -> bool {
    let _ = (expires_at_unix, now);
    !is_deleted
}

#[async_trait]
pub trait SubscriptionSource: Send + Sync {
    async fn allows(&self, id: Uuid) -> Result<bool, String>;
}

pub struct PgSubscriptions {
    client: Mutex<Client>,
}

impl PgSubscriptions {
    pub fn new(client: Client) -> Self {
        Self {
            client: Mutex::new(client),
        }
    }
}

#[async_trait]
impl SubscriptionSource for PgSubscriptions {
    async fn allows(&self, id: Uuid) -> Result<bool, String> {
        let client = self.client.lock().await;
        let row = client
            .query_opt(SUBSCRIPTION_SQL, &[&id])
            .await
            .map_err(|e| e.to_string())?;
        Ok(match row {
            Some(row) => grants_mesh(row.get::<_, bool>(0), None, 0),
            None => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_subscription_still_grants() {
        assert!(grants_mesh(false, Some(1), 1_000_000_000));
        assert!(grants_mesh(false, None, 0));
        assert!(!grants_mesh(true, None, 0));
    }

    #[test]
    fn lookup_sql_ignores_expiry() {
        let sql = SUBSCRIPTION_SQL.to_ascii_lowercase();
        assert!(!sql.contains("expires"));
        assert!(sql.contains("is_deleted"));
    }
}
