//! Wrapper over `digest::Digest::collect` for a single connection.
//!
//! Scheduling and delivery live in mrkting now, so there is nothing to claim
//! or mark here: the api only builds the data.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

use fcore::Result;

use super::pg::PgClientManager;
use crate::digest::{self, DigestKind};

pub struct PgDigest {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgDigest {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn collect(
        &self,
        kind: DigestKind,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<digest::Digest> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        Ok(digest::Digest::collect(client, kind, from, to).await?)
    }
}
