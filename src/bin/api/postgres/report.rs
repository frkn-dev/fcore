//! Приём и агрегация отчётов о блокировках (`POST /v1/report/block`).
//!
//! Сырые отчёты живут 30 дней, часовой роллап — 12 месяцев. IP пользователя в
//! открытом виде не пишем: только ASN, страна, /24 (/48 для IPv6) и blake3-хеш.

use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::Result;

use super::pg::PgClientManager;

/// Сколько живут сырые отчёты.
pub const BLOCK_REPORT_RETENTION_DAYS: i64 = 30;

#[derive(Debug, Clone)]
pub struct BlockReportRow {
    pub id: uuid::Uuid,
    pub subscription_id: Option<uuid::Uuid>,
    pub share_token_hash: Option<Vec<u8>>,
    pub support_tag: Option<String>,
    pub app_version: String,
    pub platform: String,
    pub os: Option<String>,
    pub node_address: std::net::IpAddr,
    pub node_id: Option<uuid::Uuid>,
    pub node_host: Option<String>,
    pub port: i32,
    pub protocol: String,
    pub transport: Option<String>,
    pub error_code: Option<i32>,
    pub error_stage: Option<String>,
    pub icmp_ok: Option<bool>,
    pub icmp_rtt_ms: Option<i32>,
    pub dns_ips: serde_json::Value,
    pub tcp_probe: serde_json::Value,
    pub udp_probe: serde_json::Value,
    pub device: serde_json::Value,
    pub user_note: Option<String>,
    pub asn: Option<i32>,
    pub country: Option<String>,
    pub ip_prefix: Option<String>,
    pub ip_hash: Option<Vec<u8>>,
    pub report_at: DateTime<Utc>,
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
                CREATE TABLE IF NOT EXISTS block_reports (
                    id                uuid PRIMARY KEY,
                    subscription_id   uuid,
                    share_token_hash  bytea,
                    support_tag       text,
                    app_version       text NOT NULL,
                    platform          text NOT NULL,
                    os                text,
                    node_address      inet NOT NULL,
                    node_id           uuid,
                    node_host         text,
                    port              int  NOT NULL,
                    protocol          text NOT NULL,
                    transport         text,
                    error_code        int,
                    error_stage       text,
                    icmp_ok           boolean,
                    icmp_rtt_ms       int,
                    dns_ips           jsonb,
                    tcp_probe         jsonb,
                    udp_probe         jsonb,
                    device            jsonb,
                    user_note         text,
                    asn               int,
                    country           char(2),
                    ip_prefix         cidr,
                    ip_hash           bytea,
                    report_at         timestamptz NOT NULL,
                    created_at        timestamptz NOT NULL DEFAULT now()
                );

                CREATE INDEX IF NOT EXISTS block_reports_addr_time
                    ON block_reports (node_address, port, created_at DESC);
                CREATE INDEX IF NOT EXISTS block_reports_asn_time
                    ON block_reports (asn, created_at DESC);
                CREATE INDEX IF NOT EXISTS block_reports_sub_time
                    ON block_reports (subscription_id, created_at DESC);
                "#,
            )
            .await?;

        Ok(())
    }

    /// Идемпотентная вставка: повтор с тем же `id` вернёт `false`.
    pub async fn insert_once(&self, row: &BlockReportRow) -> Result<bool> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let inserted = client
            .execute(
                r#"
                INSERT INTO block_reports (
                    id, subscription_id, share_token_hash, support_tag, app_version,
                    platform, os, node_address, node_id, node_host, port, protocol,
                    transport, error_code, error_stage, icmp_ok, icmp_rtt_ms, dns_ips,
                    tcp_probe, udp_probe, device, user_note, asn, country, ip_prefix,
                    ip_hash, report_at
                ) VALUES (
                    $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,
                    $19,$20,$21,$22,$23,$24,$25,$26,$27
                )
                ON CONFLICT (id) DO NOTHING
                "#,
                &[
                    &row.id,
                    &row.subscription_id,
                    &row.share_token_hash,
                    &row.support_tag,
                    &row.app_version,
                    &row.platform,
                    &row.os,
                    &row.node_address,
                    &row.node_id,
                    &row.node_host,
                    &row.port,
                    &row.protocol,
                    &row.transport,
                    &row.error_code,
                    &row.error_stage,
                    &row.icmp_ok,
                    &row.icmp_rtt_ms,
                    &row.dns_ips,
                    &row.tcp_probe,
                    &row.udp_probe,
                    &row.device,
                    &row.user_note,
                    &row.asn,
                    &row.country,
                    &row.ip_prefix,
                    &row.ip_hash,
                    &row.report_at,
                ],
            )
            .await?;

        Ok(inserted > 0)
    }

    /// Резолв ноды по адресу: id + hostname (у пуловых нод один хост и
    /// несколько адресов, поэтому группировать удобнее по ноде).
    pub async fn resolve_node(
        &self,
        address: std::net::IpAddr,
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

    /// Сколько отчётов по паре (адрес, порт) уже есть за окно — для дедупа по окну.
    pub async fn count_recent(
        &self,
        address: std::net::IpAddr,
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
                FROM block_reports
                WHERE node_address = $1 AND port = $2
                  AND ($3::uuid IS NULL OR subscription_id = $3)
                  AND created_at > now() - ($4 || ' seconds')::interval
                "#,
                &[&address, &port, &subscription_id, &window_secs.to_string()],
            )
            .await?;

        Ok(row.get(0))
    }

    /// Агрегат для админки по скользящему окну.
    ///
    /// `group_by` влияет на набор ключевых колонок: address (адрес+порт),
    /// port, asn, country, node.
    pub async fn aggregate(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        address: Option<std::net::IpAddr>,
        min_reports: i64,
        group_by: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let keys = match group_by {
            "port" => "port",
            "asn" => "asn",
            "country" => "country",
            "node" => "coalesce(node_host, node_address::text)",
            _ => "node_address, port",
        };

        // distinct_asn считаем по (адрес, порт), а не внутри ASN-группы.
        let sql = format!(
            r#"
            WITH base AS (
                SELECT node_address, port, protocol, coalesce(node_host, '') AS node_host,
                       asn, country, icmp_ok, tcp_probe, udp_probe, created_at
                FROM block_reports
                WHERE created_at BETWEEN $1 AND $2
                  AND ($3::inet IS NULL OR node_address = $3)
            ),
            grouped AS (
                SELECT {keys} AS group_key, * FROM base
            )
            SELECT group_key::text AS group_key,
                   count(*)::BIGINT AS reports,
                   count(DISTINCT asn)::BIGINT AS distinct_asn,
                   count(*) FILTER (WHERE icmp_ok IS FALSE)::BIGINT AS icmp_fail,
                   count(*) FILTER (
                       WHERE tcp_probe @> '[{{"state":"timeout"}}]'
                          OR tcp_probe @> '[{{"state":"refused"}}]'
                          OR tcp_probe @> '[{{"state":"err"}}]'
                   )::BIGINT AS tcp_fail,
                   count(*) FILTER (
                       WHERE udp_probe @> '[{{"state":"no-answer"}}]'
                          OR udp_probe @> '[{{"state":"err"}}]'
                   )::BIGINT AS udp_fail,
                   min(created_at) AS first_at,
                   max(created_at) AS last_at
            FROM grouped
            GROUP BY group_key
            HAVING count(*) >= $4
            ORDER BY count(*) DESC
            LIMIT 500
            "#,
            keys = keys
        );

        let rows = client
            .query(&sql, &[&from, &to, &address, &min_reports])
            .await?;

        Ok(rows
            .iter()
            .map(|r| {
                let reports: i64 = r.get("reports");
                let distinct_asn: i64 = r.get("distinct_asn");
                let tcp_fail: i64 = r.get("tcp_fail");
                let udp_fail: i64 = r.get("udp_fail");
                let fail = tcp_fail.max(udp_fail);
                let ratio = if reports > 0 {
                    fail as f64 / reports as f64
                } else {
                    0.0
                };
                // Вердикт считается здесь, а не в часовом роллапе: окно скользящее.
                let verdict = if distinct_asn >= 3 && ratio >= 0.8 {
                    "blocked"
                } else if distinct_asn >= 1 && fail >= 5 && ratio >= 0.8 {
                    "blocked_single_asn"
                } else if reports < 2 {
                    "unknown"
                } else {
                    "unknown"
                };
                serde_json::json!({
                    "group": r.get::<_, String>("group_key"),
                    "reports": reports,
                    "distinct_asn": distinct_asn,
                    "tcp_fail": tcp_fail,
                    "udp_fail": udp_fail,
                    "icmp_fail": r.get::<_, i64>("icmp_fail"),
                    "first_at": r.get::<_, DateTime<Utc>>("first_at"),
                    "last_at": r.get::<_, DateTime<Utc>>("last_at"),
                    "fail_ratio": (ratio * 100.0).round() / 100.0,
                    "verdict": verdict,
                })
            })
            .collect())
    }

    /// Ретенция сырых отчётов.
    pub async fn purge_old(&self, days: i64) -> Result<u64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let deleted = client
            .execute(
                "DELETE FROM block_reports WHERE created_at < now() - ($1 || ' days')::interval",
                &[&days.to_string()],
            )
            .await?;

        Ok(deleted)
    }
}
