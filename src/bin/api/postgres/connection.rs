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
use crate::sync::tasks::DELETE_REASON_DEVICE_KICK;

/// Marks a connection created by the tariff ensure step (activation or
/// renewal of a subscription): `connections.issued_via = 'plan'`. Distinct
/// from a user-created extra device (`NULL`) and from a share child
/// (`postgres::share::ISSUED_VIA_SHARE`). Plan-issued connections are the
/// owner's own devices: unlike share children they are listed as usual.
pub const ISSUED_VIA_PLAN: &str = "plan";
/// A peer provisioned for one installation and hidden from the subscription catalog.
pub const ISSUED_VIA_INSTALLATION: &str = "installation";

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
    /// Provenance flag: `'share'` marks a child connection minted for a share
    /// token recipient, `'plan'` marks a connection created by the tariff
    /// ensure step (activation/renewal of a subscription). PG-only like
    /// `label`; the api mirrors `'share'` into an in-memory set to hide share
    /// children from owner-facing listings — `'plan'` connections are the
    /// user's own devices and stay visible.
    pub issued_via: Option<String>,
    /// Pin of a "named device" connection to a single node (nodes.uuid).
    /// PG-only like `label`; the api mirrors it into an in-memory map so
    /// pinned peers are only published to/listed on their node. None =
    /// env-wide (current behavior).
    pub node_id: Option<uuid::Uuid>,
    /// PG-only feed visibility flag; never crosses the rkyv wire to nodes.
    pub include_in_main: bool,
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
            // Same for the node pin: callers that have one set it on the
            // row explicitly (see SyncOp::add_conn).
            node_id: None,
            include_in_main: true,
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
            env: row.env.parse::<fcore::Env>().map_err(|e| {
                Error::Custom(format!("connection row has unparseable env {:?}: {e}", row.env))
            })?,
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

pub struct InstallationConnections {
    pub revoked: bool,
    pub connections: Vec<(uuid::Uuid, uuid::Uuid)>,
}

pub struct PgConn {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgConn {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn ensure_schema(&self) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .batch_execute(
                "ALTER TABLE connections ADD COLUMN IF NOT EXISTS include_in_main BOOLEAN NOT NULL DEFAULT TRUE",
            )
            .await?;
        // Selfhosted account caps (server_limit/user_limit): applied here so
        // an existing production DB gets them on the next api start.
        client
            .batch_execute(
                "ALTER TABLE subscriptions
                     ADD COLUMN IF NOT EXISTS server_limit INTEGER NOT NULL DEFAULT 3,
                     ADD COLUMN IF NOT EXISTS user_limit INTEGER NOT NULL DEFAULT 20",
            )
            .await?;
        Ok(())
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
            issued_via,
            node_id,
            include_in_main
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
                let node_id: Option<uuid::Uuid> = row.get("node_id");
                let include_in_main: bool = row.get("include_in_main");

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
                    node_id,
                    include_in_main,
                }
            })
            .collect()
    }

    pub async fn delete(&self, conn_id: &uuid::Uuid, reason: Option<&str>) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let query = "UPDATE connections SET is_deleted = true, deleted_reason = $2 WHERE id = $1";

        client.execute(query, &[conn_id, &reason]).await?;

        Ok(())
    }

    pub async fn restore(&self, conn_id: &uuid::Uuid) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        // The reason only matters while the connection is deleted; a revived
        // connection starts clean so a later delete records a fresh reason.
        let query =
            "UPDATE connections SET is_deleted = false, deleted_reason = NULL WHERE id = $1";

        client.execute(query, &[conn_id]).await?;

        Ok(())
    }

    /// Soft-deletion reasons of all currently deleted connections of a
    /// subscription. The restore flow gates revivals on this.
    pub async fn deleted_reasons_for_subscription(
        &self,
        subscription_id: &uuid::Uuid,
    ) -> Result<Vec<(uuid::Uuid, Option<String>)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                "SELECT id, deleted_reason FROM connections WHERE subscription_id = $1 AND is_deleted",
                &[subscription_id],
            )
            .await?;

        Ok(rows
            .into_iter()
            .map(|row| (row.get("id"), row.get("deleted_reason")))
            .collect())
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

    pub async fn set_include_in_main(&self, conn_id: &uuid::Uuid, include: bool) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .execute(
                "UPDATE connections SET include_in_main = $2 WHERE id = $1",
                &[conn_id, &include],
            )
            .await?;
        Ok(())
    }

    pub async fn insert(&self, conn: ConnRow) -> Result<()> {
        if conn.issued_via.as_deref() == Some(ISSUED_VIA_INSTALLATION) {
            return Err(Error::Custom(
                "Installation connections require a device binding".into(),
            ));
        }
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        Self::insert_row(&*client, &conn).await
    }

    async fn insert_row(
        client: &(impl tokio_postgres::GenericClient + Sync),
        conn: &ConnRow,
    ) -> Result<()> {
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
            issued_via,
            node_id,
            include_in_main
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15, $16
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
                    &conn.node_id,
                    &conn.include_in_main,
                ],
            )
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                if let Some(db_err) = e.as_db_error() {
                    if db_err.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION {
                        return Err(Error::Custom(format!(
                    "Unique violation inserting connection {}: constraint={:?}, detail={:?}",
                    conn.conn_id,
                    db_err.constraint(),
                    db_err.detail(),
                )));
                    }
                }

                Err(Error::Database(e))
            }
        }
    }

    pub async fn installation_connections(
        &self,
        subscription_id: &uuid::Uuid,
        installation_id: &uuid::Uuid,
    ) -> Result<Option<InstallationConnections>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client
            .query(
                "SELECT i.revoked_at IS NOT NULL AS revoked, c.base_conn_id, c.conn_id
                 FROM device_installations i
                 LEFT JOIN device_installation_connections c USING (sub_id, installation_uuid)
                 WHERE i.sub_id = $1 AND i.installation_uuid = $2",
                &[subscription_id, installation_id],
            )
            .await?;

        Ok(rows.first().map(|first| InstallationConnections {
            revoked: first.get("revoked"),
            connections: rows
                .iter()
                .filter_map(|row| {
                    let base_conn_id: Option<uuid::Uuid> = row.get("base_conn_id");
                    let conn_id: Option<uuid::Uuid> = row.get("conn_id");
                    base_conn_id.zip(conn_id)
                })
                .collect(),
        }))
    }

    pub async fn list_installations(
        &self,
        subscription_id: &uuid::Uuid,
    ) -> Result<Vec<(uuid::Uuid, bool)>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client
            .query(
                "SELECT installation_uuid, revoked_at IS NOT NULL AS revoked
                 FROM device_installations WHERE sub_id = $1 ORDER BY installation_uuid",
                &[subscription_id],
            )
            .await?;

        Ok(rows
            .into_iter()
            .map(|row| (row.get("installation_uuid"), row.get("revoked")))
            .collect())
    }

    pub async fn insert_installation_connection(
        &self,
        conn: ConnRow,
        installation_id: uuid::Uuid,
        base_conn_id: uuid::Uuid,
    ) -> Result<()> {
        let subscription_id = conn.subscription_id.ok_or_else(|| {
            Error::Custom("Installation connection must have a subscription".into())
        })?;
        if conn.is_deleted
            || conn.include_in_main
            || conn.issued_via.as_deref() != Some(ISSUED_VIA_INSTALLATION)
            || !matches!(conn.proto, Tag::AmneziaWg | Tag::AmneziaWgMobile)
        {
            return Err(Error::Custom("Invalid installation connection".into()));
        }
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let tx = client.transaction().await?;

        // Take the subscription lock before the installation lock here and on revoke.
        if tx
            .query_opt(
                "SELECT id FROM subscriptions
                 WHERE id = $1 AND NOT is_deleted
                   AND parent_id IS NOT NULL AND plan_kind = 'selfhosted'
                   AND (expires_at IS NULL OR expires_at > now())
                 FOR UPDATE",
                &[&subscription_id],
            )
            .await?
            .is_none()
        {
            return Err(Error::Custom(
                "Installation subscription is not active".into(),
            ));
        }
        tx.execute(
            "INSERT INTO device_installations (sub_id, installation_uuid)
             VALUES ($1, $2) ON CONFLICT (sub_id, installation_uuid) DO NOTHING",
            &[&subscription_id, &installation_id],
        )
        .await?;
        let revoked: bool = tx
            .query_one(
                "SELECT revoked_at IS NOT NULL AS revoked FROM device_installations
                 WHERE sub_id = $1 AND installation_uuid = $2 FOR UPDATE",
                &[&subscription_id, &installation_id],
            )
            .await?
            .get("revoked");
        if revoked {
            return Err(Error::Custom("Installation was revoked".into()));
        }

        let base = tx
            .query_opt(
                "SELECT env FROM connections
                 WHERE id = $1 AND subscription_id = $2 AND NOT is_deleted
                   AND (expires_at IS NULL OR expires_at > now())
                   AND proto = $3 AND node_id IS NOT DISTINCT FROM $4
                   AND include_in_main
                   AND issued_via IS DISTINCT FROM 'share'
                   AND issued_via IS DISTINCT FROM 'shared_sub'
                   AND NOT EXISTS (
                       SELECT 1 FROM device_installation_connections WHERE conn_id = $1
                   )
                 FOR UPDATE",
                &[&base_conn_id, &subscription_id, &conn.proto, &conn.node_id],
            )
            .await?
            .ok_or_else(|| Error::Custom("Installation base connection is not active".into()))?;
        let base_env: String = base.get("env");
        if base_env.parse::<fcore::Env>()? != conn.env.parse::<fcore::Env>()? {
            return Err(Error::Custom(
                "Installation connection environment mismatch".into(),
            ));
        }

        Self::insert_row(&tx, &conn).await?;
        tx.execute(
            "INSERT INTO device_installation_connections
             (sub_id, installation_uuid, base_conn_id, conn_id)
             VALUES ($1, $2, $3, $4)",
            &[
                &subscription_id,
                &installation_id,
                &base_conn_id,
                &conn.conn_id,
            ],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn revoke_installation(
        &self,
        subscription_id: &uuid::Uuid,
        installation_id: &uuid::Uuid,
    ) -> Result<Vec<uuid::Uuid>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let tx = client.transaction().await?;
        tx.query_one(
            "SELECT id FROM subscriptions
             WHERE id = $1 AND parent_id IS NOT NULL AND plan_kind = 'selfhosted'
             FOR UPDATE",
            &[subscription_id],
        )
        .await?;
        tx.execute(
            "INSERT INTO device_installations (sub_id, installation_uuid, revoked_at)
             VALUES ($1, $2, now())
             ON CONFLICT (sub_id, installation_uuid) DO UPDATE
             SET revoked_at = COALESCE(device_installations.revoked_at, EXCLUDED.revoked_at)",
            &[subscription_id, installation_id],
        )
        .await?;
        tx.execute(
            "UPDATE connections c SET is_deleted = true, deleted_reason = $3, modified_at = now()
             FROM device_installation_connections d
             WHERE d.sub_id = $1 AND d.installation_uuid = $2 AND c.id = d.conn_id
               AND (NOT c.is_deleted OR c.deleted_reason IS DISTINCT FROM $3)",
            &[subscription_id, installation_id, &DELETE_REASON_DEVICE_KICK],
        )
        .await?;
        // Include previously deleted connections so retries can finish publishing revocations.
        let rows = tx
            .query(
                "SELECT conn_id FROM device_installation_connections
                 WHERE sub_id = $1 AND installation_uuid = $2",
                &[subscription_id, installation_id],
            )
            .await?;
        tx.commit().await?;
        Ok(rows.into_iter().map(|row| row.get("conn_id")).collect())
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
                  AND NOT is_deleted
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
