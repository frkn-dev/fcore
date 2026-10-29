//! История «кто куда ходил»: выходы (exit_usage) и входы (entry_usage).
//!
//! Выход: раз в час снимаем срез живых коннектов прямо в SQL — «подписка → нода →
//! IP выхода». PG здесь источник истины, память процесса не нужна.
//! Вход: нода присылает endpoint'ы пиров; IP клиента не храним — только
//! префикс /24 (/48), ASN, страну и хеш, как в телеметрии блокировок.

use chrono::{DateTime, NaiveDate, Utc};
use std::sync::Arc;
use tokio::sync::Mutex;

use fcore::Result;

use super::pg::PgClientManager;

/// Сколько дней держим историю использования.
pub const USAGE_RETENTION_DAYS: i64 = 400;

#[derive(Debug, Clone)]
pub struct EntryRow {
    pub subscription_id: Option<uuid::Uuid>,
    pub node_id: uuid::Uuid,
    pub proto: Option<String>,
    pub entry_prefix: String,
    pub asn: Option<i32>,
    pub country: Option<String>,
    pub ip_hash: Option<String>,
}

pub struct PgUsage {
    manager: Arc<Mutex<PgClientManager>>,
}

impl PgUsage {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn ensure_tables(&self) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS exit_usage (
                    day             date NOT NULL,
                    subscription_id uuid NOT NULL,
                    node_id         uuid NOT NULL,
                    exit_ip         inet NOT NULL,
                    proto           text NOT NULL,
                    samples         bigint NOT NULL DEFAULT 0,
                    first_seen      timestamptz NOT NULL,
                    last_seen       timestamptz NOT NULL,
                    PRIMARY KEY (day, subscription_id, node_id, proto)
                );

                CREATE INDEX IF NOT EXISTS exit_usage_node_day ON exit_usage (node_id, day DESC);
                CREATE INDEX IF NOT EXISTS exit_usage_ip_day   ON exit_usage (exit_ip, day DESC);

                CREATE TABLE IF NOT EXISTS entry_usage (
                    day             date NOT NULL,
                    node_id         uuid NOT NULL,
                    subscription_id uuid NOT NULL,
                    proto           text,
                    entry_prefix    cidr NOT NULL,
                    asn             integer,
                    country         char(2),
                    ip_hash         text,
                    samples         bigint NOT NULL DEFAULT 0,
                    last_seen       timestamptz NOT NULL,
                    PRIMARY KEY (day, node_id, subscription_id, entry_prefix)
                );

                CREATE INDEX IF NOT EXISTS entry_usage_node_day ON entry_usage (node_id, day DESC);
                CREATE INDEX IF NOT EXISTS entry_usage_asn_day  ON entry_usage (asn, day DESC);
                "#,
            )
            .await?;
        Ok(())
    }

    /// Часовой срез выходов: все живые коннекты → их нода → IP ноды.
    pub async fn snapshot_exit(&self) -> Result<u64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let touched = client
            .execute(
                r#"
                INSERT INTO exit_usage (day, subscription_id, node_id, exit_ip, proto, samples, first_seen, last_seen)
                SELECT current_date,
                       c.subscription_id,
                       c.node_id,
                       n.address,
                       c.proto::text,
                       count(*)::bigint,
                       now(),
                       now()
                FROM connections c
                JOIN nodes n ON n.id = c.node_id
                WHERE c.is_deleted = false
                  AND c.subscription_id IS NOT NULL
                  AND (c.expires_at IS NULL OR c.expires_at > now())
                GROUP BY 1, 2, 3, 4, 5
                ON CONFLICT (day, subscription_id, node_id, proto)
                DO UPDATE SET samples = exit_usage.samples + EXCLUDED.samples,
                              last_seen = now(),
                              exit_ip = EXCLUDED.exit_ip
                "#,
                &[],
            )
            .await?;
        Ok(touched)
    }

    /// Наблюдения о входах (то, что пришлёт агент ноды).
    pub async fn record_entry(&self, rows: &[EntryRow], at: DateTime<Utc>) -> Result<u64> {
        if rows.is_empty() {
            return Ok(0);
        }
        let zero = uuid::Uuid::nil();
        let mut subs = Vec::with_capacity(rows.len());
        let mut nodes = Vec::with_capacity(rows.len());
        let mut protos = Vec::with_capacity(rows.len());
        let mut prefixes = Vec::with_capacity(rows.len());
        let mut asns = Vec::with_capacity(rows.len());
        let mut countries = Vec::with_capacity(rows.len());
        let mut hashes = Vec::with_capacity(rows.len());
        for r in rows {
            subs.push(r.subscription_id.unwrap_or(zero));
            nodes.push(r.node_id);
            protos.push(r.proto.clone());
            prefixes.push(r.entry_prefix.clone());
            asns.push(r.asn);
            countries.push(r.country.clone());
            hashes.push(r.ip_hash.clone());
        }

        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let touched = client
            .execute(
                r#"
                INSERT INTO entry_usage (day, node_id, subscription_id, proto, entry_prefix, asn, country, ip_hash, samples, last_seen)
                SELECT $1::date, n, s, p, pre::cidr, a, c::char(2), h, 1, $2
                FROM UNNEST($3::uuid[], $4::uuid[], $5::text[], $6::text[], $7::int[], $8::text[], $9::text[])
                     AS t(s, n, p, pre, a, c, h)
                ON CONFLICT (day, node_id, subscription_id, entry_prefix)
                DO UPDATE SET samples = entry_usage.samples + 1,
                              last_seen = EXCLUDED.last_seen,
                              asn = COALESCE(EXCLUDED.asn, entry_usage.asn),
                              country = COALESCE(EXCLUDED.country, entry_usage.country)
                "#,
                &[
                    &at.date_naive(),
                    &at,
                    &subs,
                    &nodes,
                    &protos,
                    &prefixes,
                    &asns,
                    &countries,
                    &hashes,
                ],
            )
            .await?;
        Ok(touched)
    }

    pub async fn aggregate_exit(
        &self,
        from: NaiveDate,
        to: NaiveDate,
        group_by: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let key = match group_by {
            "node" => "n.hostname",
            "country" => "COALESCE(n.country, '??')",
            "subscription" => "e.subscription_id::text",
            _ => "host(e.exit_ip)",
        };
        let sql = format!(
            r#"
            SELECT {key} AS grp,
                   count(DISTINCT e.subscription_id)::bigint AS subscriptions,
                   sum(e.samples)::bigint AS samples,
                   min(e.first_seen) AS first_seen,
                   max(e.last_seen) AS last_seen
            FROM exit_usage e
            LEFT JOIN nodes n ON n.id = e.node_id
            WHERE e.day BETWEEN $1 AND $2
            GROUP BY 1
            ORDER BY subscriptions DESC, samples DESC
            LIMIT 200
            "#
        );
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client.query(&sql, &[&from, &to]).await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "group": r.get::<_, String>("grp"),
                    "subscriptions": r.get::<_, i64>("subscriptions"),
                    "samples": r.get::<_, i64>("samples"),
                    "first_seen": r.get::<_, DateTime<Utc>>("first_seen"),
                    "last_seen": r.get::<_, DateTime<Utc>>("last_seen"),
                })
            })
            .collect())
    }

    pub async fn aggregate_entry(
        &self,
        from: NaiveDate,
        to: NaiveDate,
        group_by: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let key = match group_by {
            "node" => "n.hostname",
            "country" => "COALESCE(e.country::text, '??')",
            "asn" => "COALESCE('AS' || e.asn::text, 'AS?')",
            "prefix" => "host(e.entry_prefix)",
            _ => "COALESCE('AS' || e.asn::text, 'AS?') || ' ' || COALESCE(e.country::text, '??')",
        };
        let sql = format!(
            r#"
            SELECT {key} AS grp,
                   count(DISTINCT e.subscription_id)::bigint AS subscriptions,
                   sum(e.samples)::bigint AS samples,
                   max(e.last_seen) AS last_seen
            FROM entry_usage e
            LEFT JOIN nodes n ON n.id = e.node_id
            WHERE e.day BETWEEN $1 AND $2
              AND e.subscription_id <> '00000000-0000-0000-0000-000000000000'::uuid
            GROUP BY 1
            ORDER BY subscriptions DESC, samples DESC
            LIMIT 200
            "#
        );
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client.query(&sql, &[&from, &to]).await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "group": r.get::<_, String>("grp"),
                    "subscriptions": r.get::<_, i64>("subscriptions"),
                    "samples": r.get::<_, i64>("samples"),
                    "last_seen": r.get::<_, DateTime<Utc>>("last_seen"),
                })
            })
            .collect())
    }

    pub async fn purge_old(&self, days: i64) -> Result<u64> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let mut deleted = client
            .execute(
                "DELETE FROM exit_usage WHERE day < (now() - ($1 || ' days')::interval)::date",
                &[&days.to_string()],
            )
            .await?;
        deleted += client
            .execute(
                "DELETE FROM entry_usage WHERE day < (now() - ($1 || ' days')::interval)::date",
                &[&days.to_string()],
            )
            .await?;
        Ok(deleted)
    }
}
