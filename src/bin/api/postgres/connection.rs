use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::{
    Connection, ConnectionBaseOperations, Error, IpAddrMask, Proto, Result, Tag, WgKeys, WgParam,
};

use super::pg::PgClientManager;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ConnRow {
    pub conn_id: uuid::Uuid,
    pub password: Option<String>,
    pub env: String,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub subscription_id: Option<uuid::Uuid>,
    pub wg: Option<WgParam>,
    pub proto: Tag,
    pub token: Option<uuid::Uuid>,
    is_deleted: bool,
    /// User-facing name of a "named device" connection. PG-only: never put
    /// into `Connection`/`Conn` so it cannot cross the rkyv wire to nodes
    /// (same precedent as `deleted_reason`).
    pub label: Option<String>,
    /// Provenance flag: Some("share") marks a child connection minted for a
    /// share token recipient. PG-only like `label`; the api mirrors it into
    /// an in-memory set to hide share children from owner-facing listings.
    pub issued_via: Option<String>,
}

impl From<(uuid::Uuid, Connection)> for ConnRow {
    fn from((conn_id, conn): (uuid::Uuid, Connection)) -> Self {
        let wg = conn.get_wireguard().or_else(|| conn.get_amneziawg());
        ConnRow {
            conn_id,
            password: conn.get_password(),
            env: conn.env.to_string(),
            created_at: conn.created_at,
            modified_at: conn.modified_at,
            expires_at: conn.expires_at,
            subscription_id: conn.subscription_id,
            wg: wg.cloned(),
            proto: conn.get_proto().proto(),
            token: conn.get_token(),
            is_deleted: conn.is_deleted,
            // The label never lives on `Connection`; callers that have one
            // set it on the row explicitly (see SyncOp::add_conn).
            label: None,
            // Same for the share provenance flag: set post-insert by the
            // share mint flow (PgConn::set_issued_via).
            issued_via: None,
        }
    }
}

impl TryFrom<ConnRow> for Connection {
    type Error = Error;

    fn try_from(row: ConnRow) -> Result<Self> {
        let proto = match row.proto {
            Tag::AmneziaWg => {
                let wg = row
                    .wg
                    .ok_or_else(|| Error::Custom("Missing Wireguard param".into()))?;

                Proto::new_awg(&wg)
            }

            Tag::AmneziaWgMobile => {
                let wg = row
                    .wg
                    .ok_or_else(|| Error::Custom("Missing Wireguard param".into()))?;

                Proto::new_awg_mobile(&wg)
            }

            Tag::Wireguard => {
                let wg = row
                    .wg
                    .ok_or_else(|| Error::Custom("Missing Wireguard param".into()))?;

                Proto::new_wg(&wg)
            }

            Tag::Shadowsocks => {
                let password = row
                    .password
                    .ok_or_else(|| Error::Custom("Missing Shadowsocks password".into()))?;

                Proto::new_ss(&password)
            }

            Tag::Hysteria2 => {
                let token = row
                    .token
                    .ok_or_else(|| Error::Custom("Missing Hysteria2 token".into()))?;
                Proto::new_hysteria2(&token)
            }

            tag => Proto::new_xray(&tag),
        };

        Ok(Self {
            env: row.env.into(),
            proto,
            subscription_id: row.subscription_id,
            created_at: row.created_at,
            modified_at: row.modified_at,
            expires_at: row.expires_at,
            is_deleted: row.is_deleted,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ConnWatermark {
    pub conn_id: uuid::Uuid,
    pub subscription_id: uuid::Uuid,
    pub env: String,
    pub uplink: i64,
    pub downlink: i64,
    pub last_persist_at: DateTime<Utc>,
}

pub struct PgConn {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgConn {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn all(&self) -> Result<Vec<ConnRow>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let query = "
        SELECT
            id,
            password,
            token,
            env,
            created_at,
            modified_at,
            expires_at,
            subscription_id,
            proto,
            wg_privkey,
            wg_address,
            is_deleted,
            label,
            issued_via
        FROM connections
    ";

        let rows = client.query(query, &[]).await?;
        let conns = self.map_rows_to_conns(rows);
        Ok(conns)
    }

    fn map_rows_to_conns(&self, rows: Vec<tokio_postgres::Row>) -> Vec<ConnRow> {
        rows.into_iter()
            .map(|row| {
                let conn_id: uuid::Uuid = row.get("id");
                let password: Option<String> = row.get("password");
                let env: String = row.get("env");
                let created_at: DateTime<Utc> = row.get("created_at");
                let modified_at: DateTime<Utc> = row.get("modified_at");
                let expires_at: Option<DateTime<Utc>> = row.get("expires_at");
                let subscription_id: Option<uuid::Uuid> = row.get("subscription_id");
                let token: Option<uuid::Uuid> = row.get("token");
                let proto: Tag = row.get("proto");
                let wg_privkey: Option<String> = row.get("wg_privkey");
                let wg_address: Option<String> = row.get("wg_address");
                let is_deleted: bool = row.get("is_deleted");
                let label: Option<String> = row.get("label");
                let issued_via: Option<String> = row.get("issued_via");

                let wg = match (wg_privkey, wg_address) {
                    (Some(privkey), Some(address)) => {
                        address.parse::<IpAddrMask>().ok().map(|ip_mask| WgParam {
                            keys: WgKeys { privkey },
                            address: ip_mask,
                        })
                    }
                    _ => None,
                };

                ConnRow {
                    conn_id,
                    password,
                    token,
                    env,
                    created_at,
                    modified_at,
                    expires_at,
                    subscription_id,
                    proto,
                    wg,
                    is_deleted,
                    label,
                    issued_via,
                }
            })
            .collect()
    }

    pub async fn delete(&self, conn_id: &uuid::Uuid) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let query = "UPDATE connections SET is_deleted = true WHERE id = $1";

        client.execute(query, &[conn_id]).await?;

        Ok(())
    }

    pub async fn restore(&self, conn_id: &uuid::Uuid) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let query = "UPDATE connections SET is_deleted = false WHERE id = $1";

        client.execute(query, &[conn_id]).await?;

        Ok(())
    }

    /// Sets the PG-only provenance flag (e.g. "share" for minted child
    /// connections). Called post-insert by the share mint flow.
    pub async fn set_issued_via(&self, conn_id: &uuid::Uuid, issued_via: &str) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let query = "UPDATE connections SET issued_via = $2 WHERE id = $1";

        client.execute(query, &[conn_id, &issued_via]).await?;

        Ok(())
    }

    pub async fn insert(&self, conn: ConnRow) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let query = "
        INSERT INTO connections (
            id,
            password,
            env,
            created_at,
            modified_at,
            expires_at,
            subscription_id,
            proto,
            is_deleted,
            wg_privkey,
            wg_address,
            token,
            label,
            issued_via
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14
        )
    ";

        let result = client
            .execute(
                query,
                &[
                    &conn.conn_id,
                    &conn.password,
                    &conn.env,
                    &conn.created_at,
                    &conn.modified_at,
                    &conn.expires_at,
                    &conn.subscription_id,
                    &conn.proto,
                    &conn.is_deleted,
                    &conn.wg.as_ref().map(|w| &w.keys.privkey),
                    &conn.wg.as_ref().map(|w| w.address.to_string()),
                    &conn.token,
                    &conn.label,
                    &conn.issued_via,
                ],
            )
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                if let Some(code) = e.code() {
                    if code == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION {
                        return Err(Error::Custom(format!(
                            "Connection {} already exists",
                            conn.conn_id
                        )));
                    }
                }

                Err(Error::Database(e))
            }
        }
    }

    pub async fn all_watermarks(&self) -> Result<Vec<ConnWatermark>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT
                    id,
                    subscription_id,
                    env,
                    uplink,
                    downlink,
                    COALESCE(last_traffic_persist_at, created_at) AS last_traffic_persist_at
                FROM connections
                WHERE subscription_id IS NOT NULL
                "#,
                &[],
            )
            .await?;

        Ok(rows.into_iter().map(Self::map_watermark_row).collect())
    }

    pub async fn watermarks_for_subscription(
        &self,
        subscription_id: uuid::Uuid,
    ) -> Result<Vec<ConnWatermark>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT
                    id,
                    subscription_id,
                    env,
                    uplink,
                    downlink,
                    COALESCE(last_traffic_persist_at, created_at) AS last_traffic_persist_at
                FROM connections
                WHERE subscription_id = $1
                "#,
                &[&subscription_id],
            )
            .await?;

        Ok(rows.into_iter().map(Self::map_watermark_row).collect())
    }

    fn map_watermark_row(row: tokio_postgres::Row) -> ConnWatermark {
        ConnWatermark {
            conn_id: row.get("id"),
            subscription_id: row.get("subscription_id"),
            env: row.get("env"),
            uplink: row.get::<_, Option<i64>>("uplink").unwrap_or(0),
            downlink: row.get::<_, Option<i64>>("downlink").unwrap_or(0),
            last_persist_at: row
                .get::<_, Option<DateTime<Utc>>>("last_traffic_persist_at")
                .unwrap_or_else(Utc::now),
        }
    }

    pub async fn update_watermark(
        &self,
        conn_id: uuid::Uuid,
        uplink: i64,
        downlink: i64,
        last_persist_at: DateTime<Utc>,
    ) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .execute(
                "UPDATE connections SET uplink = $1, downlink = $2, last_traffic_persist_at = $3 WHERE id = $4",
                &[&uplink, &downlink, &last_persist_at, &conn_id],
            )
            .await?;

        Ok(())
    }
}
