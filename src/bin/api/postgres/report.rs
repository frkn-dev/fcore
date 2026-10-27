//! Приём наблюдений о доступности узлов (`/v1/report/block`, `/v1/report/scan`).
//!
//! Оба эндпойнта пишут в одну таблицу `block_observations`: единица хранения —
//! «одна цель в одном отчёте». Так агрегат и вердикты считаются одним запросом
//! и по одиночным отчётам, и по срезам «Check all servers».
//!
//! IP пользователя не храним: только ASN, страна, префикс /24 (/48) и хеш для
//! счёта уникальных.

use chrono::{DateTime, Utc};
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::Result;

use super::pg::PgClientManager;

/// Ретенция сырых наблюдений.
pub const BLOCK_OBSERVATION_RETENTION_DAYS: i64 = 30;

/// Состояния проб, которые считаем «цель недоступна».
pub const FAIL_STATES: &[&str] = &["timeout", "refused", "no-answer", "err", "no-host"];
/// Состояния «цель отвечает».
pub const OK_STATES: &[&str] = &["open", "answered"];

#[derive(Debug, Clone)]
pub struct Observation {
    pub id: uuid::Uuid,
    pub subscription_id: Option<uuid::Uuid>,
    pub source: String,
    pub batch_id: uuid::Uuid,
    pub support_tag: Option<String>,
    pub app_version: String,
    pub platform: String,
    pub os: Option<String>,
    pub target_host: Option<String>,
    pub address: Option<IpAddr>,
    pub node_id: Option<uuid::Uuid>,
    pub node_host: Option<String>,
    pub port: i32,
    pub protocol: String,
    pub transport: Option<String>,
    pub state: Option<String>,
    pub rtt_ms: Option<i32>,
    pub icmp_ok: Option<bool>,
    pub target_country: Option<String>,
    pub error: serde_json::Value,
    pub note: Option<String>,
    pub app: serde_json::Value,
    pub device: serde_json::Value,
    pub network: serde_json::Value,
    pub probe: serde_json::Value,
    pub session: serde_json::Value,
    pub country_code: Option<String>,
    pub asn: Option<i32>,
    pub client_country: Option<String>,
    pub ip_prefix: Option<String>,
    pub ip_hash: Option<Vec<u8>>,
    pub observed_at: DateTime<Utc>,
}

pub struct PgReport {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgReport {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn ensure_table(&self) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS block_observations (
                    id               uuid PRIMARY KEY,
                    subscription_id  uuid,
                    source           text NOT NULL,
                    batch_id         uuid NOT NULL,
                    support_tag      text,
                    app_version      text NOT NULL,
                    platform         text NOT NULL,
                    os               text,
                    target_host      text,
                    address          inet,
                    node_id          uuid,
                    node_host        text,
                    port             int  NOT NULL,
                    protocol         text NOT NULL,
                    transport        text,
                    state            text,
                    rtt_ms           int,
                    icmp_ok          boolean,
                    target_country   text,
                    error            jsonb,
                    note             text,
                    app              jsonb,
                    device           jsonb,
                    network          jsonb,
                    probe            jsonb,
                    session          jsonb,
                    country_code     char(2),
                    asn              int,
                    client_country   char(2),
                    ip_prefix        cidr,
                    ip_hash          bytea,
                    observed_at      timestamptz NOT NULL,
                    created_at       timestamptz NOT NULL DEFAULT now()
                );

                CREATE INDEX IF NOT EXISTS block_obs_addr_time
                    ON block_observations (address, port, created_at DESC);
                CREATE INDEX IF NOT EXISTS block_obs_host_time
                    ON block_observations (target_host, port, created_at DESC);
                CREATE INDEX IF NOT EXISTS block_obs_asn_time
                    ON block_observations (asn, created_at DESC);
                CREATE INDEX IF NOT EXISTS block_obs_sub_time
                    ON block_observations (subscription_id, created_at DESC);
                CREATE INDEX IF NOT EXISTS block_obs_batch
                    ON block_observations (batch_id);
                "#,
            )
            .await?;

        Ok(())
    }

    /// Вставка пачки наблюдений. Возвращает `false`, если `batch_id` уже был
    /// (идемпотентность по `report_id` / `scan_id`).
    pub async fn insert_batch(&self, batch_id: uuid::Uuid, rows: &[Observation]) -> Result<bool> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let tx = client.transaction().await?;

        let seen = tx
            .query_opt(
                "SELECT 1 FROM block_observations WHERE batch_id = $1 LIMIT 1",
                &[&batch_id],
            )
            .await?;
        if seen.is_some() {
            return Ok(false);
        }

        for row in rows {
            tx.execute(
                r#"
                INSERT INTO block_observations (
                    id, subscription_id, source, batch_id, support_tag, app_version,
                    platform, os, target_host, address, node_id, node_host, port,
                    protocol, transport, state, rtt_ms, icmp_ok, probe, session,
                    country_code, asn, client_country, ip_prefix, ip_hash, observed_at,
                    app, device, network, target_country, error, note
                ) VALUES (
                    $1,$2,$3,$4,$5,$6,$7,$8,$9,$10::inet,$11,$12,$13,$14,$15,$16,$17,$18,
                    $19,$20,$21::text::char(2),$22,$23::text::char(2),$24::text::cidr,$25,$26,
                    $27,$28,$29,$30,$31,$32
                )
                ON CONFLICT (id) DO NOTHING
                "#,
                &[
                    &row.id,
                    &row.subscription_id,
                    &row.source,
                    &row.batch_id,
                    &row.support_tag,
                    &row.app_version,
                    &row.platform,
                    &row.os,
                    &row.target_host,
                    &row.address,
                    &row.node_id,
                    &row.node_host,
                    &row.port,
                    &row.protocol,
                    &row.transport,
                    &row.state,
                    &row.rtt_ms,
                    &row.icmp_ok,
                    &row.probe,
                    &row.session,
                    &row.country_code,
                    &row.asn,
                    &row.client_country,
                    &row.ip_prefix,
                    &row.ip_hash,
                    &row.observed_at,
                    &row.app,
                    &row.device,
                    &row.network,
                    &row.target_country,
                    &row.error,
                    &row.note,
                ],
            )
            .await?;
        }

        tx.commit().await?;
        Ok(true)
    }

    /// Сколько наблюдений по паре (адрес|хост, порт) от этой подписки уже есть
    /// за окно — дедуп «тот же юзер снова жалуется на то же».
    pub async fn count_recent(
        &self,
        address: Option<IpAddr>,
        host: Option<&str>,
        port: i32,
        subscription_id: Option<uuid::Uuid>,
        window_secs: i64,
    ) -> Result<i64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let row = client
            .query_one(
                r#"
                SELECT count(*)::BIGINT
                FROM block_observations
                WHERE port = $3
                  AND ($1::inet IS NULL OR address = $1)
                  AND ($2::text IS NULL OR target_host = $2)
                  AND ($4::uuid IS NULL OR subscription_id = $4)
                  AND created_at > now() - ($5 || ' seconds')::interval
                "#,
                &[&address, &host, &port, &subscription_id, &window_secs.to_string()],
            )
            .await?;

        Ok(row.get(0))
    }

    /// Резолв ноды по адресу: id + hostname.
    pub async fn resolve_node(
        &self,
        address: IpAddr,
    ) -> Result<(Option<uuid::Uuid>, Option<String>)> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                "SELECT id, hostname FROM nodes WHERE address = $1 LIMIT 1",
                &[&address],
            )
            .await?;

        match rows.first() {
            Some(r) => Ok((Some(r.get("id")), Some(r.get("hostname")))),
            None => Ok((None, None)),
        }
    }

    /// Резолв ноды по hostname (для отчётов без адреса — DNS не ответил).
    pub async fn resolve_node_by_host(
        &self,
        host: &str,
    ) -> Result<(Option<uuid::Uuid>, Option<String>)> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                "SELECT id, hostname FROM nodes WHERE hostname = $1 LIMIT 1",
                &[&host],
            )
            .await?;

        match rows.first() {
            Some(r) => Ok((Some(r.get("id")), Some(r.get("hostname")))),
            None => Ok((None, None)),
        }
    }

    /// Агрегат для админки по скользящему окну.
    pub async fn aggregate(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        address: Option<IpAddr>,
        min_observations: i64,
        group_by: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let keys = match group_by {
            "port" => "port".to_string(),
            "asn" => "asn".to_string(),
            "country" => "client_country".to_string(),
            "node" => "coalesce(node_host, target_host, address::text)".to_string(),
            _ => "coalesce(target_host, address::text), port".to_string(),
        };

        let sql = format!(
            r#"
            WITH base AS (
                SELECT coalesce(target_host, address::text) AS target_host,
                       address, port, protocol, coalesce(node_host,'') AS node_host,
                       asn, client_country, state, icmp_ok, created_at
                FROM block_observations
                WHERE created_at BETWEEN $1 AND $2
                  AND ($3::inet IS NULL OR address = $3)
            )
            SELECT ({keys})::text AS group_key,
                   count(*)::BIGINT AS observations,
                   count(DISTINCT asn)::BIGINT AS distinct_asn,
                   count(DISTINCT ip_hash)::BIGINT AS distinct_clients,
                   count(*) FILTER (WHERE state = ANY($5))::BIGINT AS fail_count,
                   count(*) FILTER (WHERE state = ANY($6))::BIGINT AS ok_count,
                   count(*) FILTER (WHERE state = 'skipped')::BIGINT AS skipped_count,
                   count(*) FILTER (WHERE icmp_ok IS FALSE)::BIGINT AS icmp_fail,
                   min(created_at) AS first_at,
                   max(created_at) AS last_at
            FROM base
            GROUP BY group_key
            HAVING count(*) >= $4
            ORDER BY count(*) DESC
            LIMIT 500
            "#,
            keys = keys
        );

        let fail: Vec<String> = FAIL_STATES.iter().map(|s| s.to_string()).collect();
        let ok: Vec<String> = OK_STATES.iter().map(|s| s.to_string()).collect();

        let rows = client
            .query(
                &sql,
                &[&from, &to, &address, &min_observations, &fail, &ok],
            )
            .await?;

        Ok(rows
            .iter()
            .map(|r| {
                let observations: i64 = r.get("observations");
                let distinct_asn: i64 = r.get("distinct_asn");
                let distinct_clients: i64 = r.get("distinct_clients");
                let fail_count: i64 = r.get("fail_count");
                let ok_count: i64 = r.get("ok_count");
                let judged = fail_count + ok_count;
                let ratio = if judged > 0 {
                    fail_count as f64 / judged as f64
                } else {
                    0.0
                };
                // Вердикт считается здесь, а не в часовом роллапе: окно скользящее.
                let verdict = if distinct_asn >= 3 && ratio >= 0.8 {
                    "blocked"
                } else if distinct_clients >= 5 && fail_count >= 5 && ratio >= 0.8 {
                    "blocked_single_asn"
                } else {
                    "unknown"
                };
                serde_json::json!({
                    "group": r.get::<_, String>("group_key"),
                    "observations": observations,
                    "distinct_asn": distinct_asn,
                    "distinct_clients": distinct_clients,
                    "fail": fail_count,
                    "ok": ok_count,
                    "skipped": r.get::<_, i64>("skipped_count"),
                    "icmp_fail": r.get::<_, i64>("icmp_fail"),
                    "first_at": r.get::<_, DateTime<Utc>>("first_at"),
                    "last_at": r.get::<_, DateTime<Utc>>("last_at"),
                    "fail_ratio": (ratio * 100.0).round() / 100.0,
                    "verdict": verdict,
                })
            })
            .collect())
    }

    /// Ретенция.
    pub async fn purge_old(&self, days: i64) -> Result<u64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let deleted = client
            .execute(
                "DELETE FROM block_observations WHERE created_at < now() - ($1 || ' days')::interval",
                &[&days.to_string()],
            )
            .await?;

        Ok(deleted)
    }
}
