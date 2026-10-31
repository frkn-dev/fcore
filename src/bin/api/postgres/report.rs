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

/// Состояния проб, которые считаем «цель недоступна» без оговорок.
///
/// `no-answer` сюда НЕ входит: для UDP (AWG/WireGuard) сервер молча дропает
/// любой пакет, кроме валидного handshake, поэтому «нет ответа» на голую
/// датаграмму — норма, а не блокировка. Такие пробы считаем неопределёнными.
pub const FAIL_STATES: &[&str] = &["timeout", "refused", "err", "no-host", "error"];
/// Состояния «цель отвечает».
///
/// `ok` — словарь клиента 4.8.14.79+ (`targets[].state`, `attempts[].outcome`);
/// `open`/`answered` — прежний. Оба живут одновременно: сырые значения пишем как
/// пришли, а счётчики знают оба словаря. Иначе новые отчёты ложатся в базу, но
/// не попадают ни в один счётчик — и дашборд выглядит пустым, хотя данные есть.
pub const OK_STATES: &[&str] = &["open", "answered", "ok"];

/// Состояния «проба ничего не определила».
///
/// Для UDP (AWG/WireGuard) сервер молча дропает любой пакет, кроме валидного
/// handshake, поэтому «нет ответа» на голую датаграмму — норма, а не блокировка.
/// `unknown` — то же самое в словаре клиента 4.8.14.79+.
pub const INCONCLUSIVE_STATES: &[&str] = &["no-answer", "unknown"];

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
    /// Оператор клиента по AS («MegaFon»), из GeoLite2-ASN.
    pub client_org: Option<String>,
    /// Город клиента («Vladivostok»), из GeoLite2-City.
    pub client_city: Option<String>,
    pub ip_prefix: Option<String>,
    pub ip_hash: Option<Vec<u8>>,
    pub observed_at: DateTime<Utc>,
}

/// Один INSERT на строку наблюдения.
///
/// Вынесен в константу, чтобы тест сверял число колонок с числом
/// параметров: при добавлении колонки легко забыть продлить `$N`, и тогда
/// каждый отчёт отвечает 500 «db error» — ошибка видна только в проде.
const INSERT_OBSERVATION_SQL: &str = r#"
                INSERT INTO block_observations (
                    id, subscription_id, source, batch_id, support_tag, app_version,
                    platform, os, target_host, address, node_id, node_host, port,
                    protocol, transport, state, rtt_ms, icmp_ok, probe, session,
                    country_code, asn, client_country, client_org, client_city,
                    ip_prefix, ip_hash, observed_at,
                    app, device, network, target_country, error, note
                ) VALUES (
                    $1,$2,$3,$4,$5,$6,$7,$8,$9,$10::inet,$11,$12,$13,$14,$15,$16,$17,$18,
                    $19,$20,$21::text::char(2),$22,$23,$24,$25,$26::text::cidr,$27,
                    $28,$29,$30,$31,$32,$33,$34
                )
                ON CONFLICT (id) DO NOTHING
                "#;

/// Отчёт клиента целиком — «кто прислал и что у него происходит».
#[derive(Debug, Clone)]
pub struct ClientReport {
    pub id: uuid::Uuid,
    pub subscription_id: Option<uuid::Uuid>,
    pub support_tag: Option<String>,
    /// `tunnel` — проверка текущего соединения, `probe`/`full` — проверка
    /// доступных серверов, `None` — старый клиент, который режим не присылал.
    pub mode: Option<String>,
    pub app_version: String,
    pub platform: String,
    pub os: Option<String>,
    pub app: serde_json::Value,
    pub config: serde_json::Value,
    pub mtu: serde_json::Value,
    pub tunnel: serde_json::Value,
    pub network: serde_json::Value,
    pub session: serde_json::Value,
    pub device: serde_json::Value,
    /// Тело как пришло: разбирать новые поля задним числом, не меняя схему.
    pub payload: serde_json::Value,
    pub country_code: Option<String>,
    pub asn: Option<i32>,
    pub client_country: Option<String>,
    pub client_org: Option<String>,
    pub client_city: Option<String>,
    pub ip_prefix: Option<String>,
    pub ip_hash: Option<Vec<u8>>,
    pub observed_at: DateTime<Utc>,
}

/// Одна попытка подключения: «сюда не смогли, столько ждали, вот код».
#[derive(Debug, Clone)]
pub struct ConnectionAttempt {
    pub id: uuid::Uuid,
    pub report_id: uuid::Uuid,
    pub subscription_id: Option<uuid::Uuid>,
    pub support_tag: Option<String>,
    pub app_version: String,
    pub platform: String,
    pub address: Option<IpAddr>,
    pub port: Option<i32>,
    pub protocol: Option<String>,
    pub outcome: Option<String>,
    pub detail: Option<String>,
    pub waited_ms: Option<i32>,
    pub client_country: Option<String>,
    pub client_org: Option<String>,
    pub ip_hash: Option<Vec<u8>>,
    pub attempted_at: DateTime<Utc>,
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
                    client_org       text,
                    client_city      text,
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

                -- Один отчёт целиком: кто прислал и в каком состоянии у него
                -- было приложение. Отдельно от наблюдений, потому что это не
                -- «цель отвечает/нет», а «вот что у клиента происходит»:
                -- режим (tunnel/probe), конфиг, MTU, туннель, сессия.
                CREATE TABLE IF NOT EXISTS client_reports (
                    id               uuid PRIMARY KEY,
                    subscription_id  uuid,
                    support_tag      text,
                    mode             text,
                    app_version      text NOT NULL,
                    platform         text NOT NULL,
                    os               text,
                    app              jsonb,
                    config           jsonb,
                    mtu              jsonb,
                    tunnel           jsonb,
                    network          jsonb,
                    session          jsonb,
                    device           jsonb,
                    payload          jsonb NOT NULL,
                    country_code     text,
                    asn              int,
                    client_country   char(2),
                    client_org       text,
                    client_city      text,
                    ip_prefix        cidr,
                    ip_hash          bytea,
                    observed_at      timestamptz NOT NULL,
                    created_at       timestamptz NOT NULL DEFAULT now()
                );

                CREATE INDEX IF NOT EXISTS client_reports_tag_time
                    ON client_reports (support_tag, created_at DESC);
                CREATE INDEX IF NOT EXISTS client_reports_sub_time
                    ON client_reports (subscription_id, created_at DESC);
                CREATE INDEX IF NOT EXISTS client_reports_time
                    ON client_reports (created_at DESC);

                -- Журнал попыток подключения: что именно не получилось и
                -- сколько юзер ждал. Это не наблюдение за целью (у попытки нет
                -- «проверяемой цели», есть результат), поэтому отдельная
                -- таблица: смешивать их в агрегатах — значит получить кашу.
                CREATE TABLE IF NOT EXISTS connection_attempts (
                    id               uuid PRIMARY KEY,
                    report_id        uuid NOT NULL,
                    subscription_id  uuid,
                    support_tag      text,
                    app_version      text NOT NULL,
                    platform         text NOT NULL,
                    address          inet,
                    port             int,
                    protocol         text,
                    outcome          text,
                    detail           text,
                    waited_ms        int,
                    client_country   char(2),
                    client_org       text,
                    ip_hash          bytea,
                    attempted_at     timestamptz NOT NULL,
                    created_at       timestamptz NOT NULL DEFAULT now()
                );

                CREATE INDEX IF NOT EXISTS conn_attempts_tag_time
                    ON connection_attempts (support_tag, created_at DESC);
                CREATE INDEX IF NOT EXISTS conn_attempts_addr_time
                    ON connection_attempts (address, port, created_at DESC);
                CREATE INDEX IF NOT EXISTS conn_attempts_report
                    ON connection_attempts (report_id);
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
                INSERT_OBSERVATION_SQL,
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
                    &row.client_org,
                    &row.client_city,
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
                &[
                    &address,
                    &host,
                    &port,
                    &subscription_id,
                    &window_secs.to_string(),
                ],
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

    /// Сырые наблюдения для поддержки: по подписке, по адресу или просто потоком.
    ///
    /// Это то, что нужно, когда юзер пишет «я отправил отчёт» — открыть его и
    /// увидеть по каждой цели: хост, порт, протокол, состояние, RTT, ICMP,
    /// версию приложения и с какой ноды смотрели.
    pub async fn observations(
        &self,
        subscription_id: Option<uuid::Uuid>,
        address: Option<std::net::IpAddr>,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client
            .query(
                r#"
                SELECT created_at, source, subscription_id, support_tag, app_version, platform,
                       target_host, host(address) AS addr, port, protocol, transport, state,
                       rtt_ms, icmp_ok, node_host, asn, client_country,
                       client_org, client_city, target_country,
                       note, error
                FROM block_observations
                WHERE ($1::uuid IS NULL OR subscription_id = $1)
                  AND ($2::inet IS NULL OR address = $2)
                ORDER BY created_at DESC
                LIMIT $3
                "#,
                &[&subscription_id, &address, &limit],
            )
            .await?;

        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "at": r.get::<_, DateTime<Utc>>("created_at"),
                    "source": r.get::<_, String>("source"),
                    "subscription_id": r.get::<_, Option<uuid::Uuid>>("subscription_id"),
                    "support_tag": r.get::<_, Option<String>>("support_tag"),
                    "app_version": r.get::<_, String>("app_version"),
                    "platform": r.get::<_, String>("platform"),
                    "host": r.get::<_, Option<String>>("target_host"),
                    "address": r.get::<_, Option<String>>("addr"),
                    "port": r.get::<_, i32>("port"),
                    "protocol": r.get::<_, String>("protocol"),
                    "transport": r.get::<_, Option<String>>("transport"),
                    "state": r.get::<_, Option<String>>("state"),
                    "rtt_ms": r.get::<_, Option<i32>>("rtt_ms"),
                    "icmp_ok": r.get::<_, Option<bool>>("icmp_ok"),
                    "node_host": r.get::<_, Option<String>>("node_host"),
                    "asn": r.get::<_, Option<i32>>("asn"),
                    "client_country": r.get::<_, Option<String>>("client_country"),
                    "client_org": r.get::<_, Option<String>>("client_org"),
                    "client_city": r.get::<_, Option<String>>("client_city"),
                    "target_country": r.get::<_, Option<String>>("target_country"),
                    "note": r.get::<_, Option<String>>("note"),
                    "error": r.get::<_, Option<serde_json::Value>>("error"),
                })
            })
            .collect())
    }

    /// Сохраняет отчёт целиком. Возвращает `false`, если такой уже есть
    /// (повторная отправка того же тела — клиент повторяет без сети).
    pub async fn insert_client_report(&self, report: &ClientReport) -> Result<bool> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let inserted = client
            .execute(
                r#"
                INSERT INTO client_reports (
                    id, subscription_id, support_tag, mode, app_version, platform, os,
                    app, config, mtu, tunnel, network, session, device, payload,
                    country_code, asn, client_country, client_org, client_city,
                    ip_prefix, ip_hash, observed_at
                ) VALUES (
                    $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,
                    $16,$17,$18::text::char(2),$19,$20,$21::text::cidr,$22,$23
                )
                -- Upsert, а не DO NOTHING: клиент обязан повторять один и тот же
                -- report_id при переотправке, и последняя версия должна
                -- побеждать. DO NOTHING оставил бы в базе первый, возможно
                -- неполный, вариант (оборвалась сеть на середине).
                ON CONFLICT (id) DO UPDATE SET
                    support_tag     = EXCLUDED.support_tag,
                    mode            = EXCLUDED.mode,
                    app             = EXCLUDED.app,
                    config          = EXCLUDED.config,
                    mtu             = EXCLUDED.mtu,
                    tunnel          = EXCLUDED.tunnel,
                    network         = EXCLUDED.network,
                    session         = EXCLUDED.session,
                    device          = EXCLUDED.device,
                    payload         = EXCLUDED.payload,
                    observed_at     = EXCLUDED.observed_at
                "#,
                &[
                    &report.id,
                    &report.subscription_id,
                    &report.support_tag,
                    &report.mode,
                    &report.app_version,
                    &report.platform,
                    &report.os,
                    &report.app,
                    &report.config,
                    &report.mtu,
                    &report.tunnel,
                    &report.network,
                    &report.session,
                    &report.device,
                    &report.payload,
                    &report.country_code,
                    &report.asn,
                    &report.client_country,
                    &report.client_org,
                    &report.client_city,
                    &report.ip_prefix,
                    &report.ip_hash,
                    &report.observed_at,
                ],
            )
            .await?;
        Ok(inserted > 0)
    }

    /// Журнал попыток подключения. Идемпотентно по `id`.
    pub async fn insert_attempts(&self, rows: &[ConnectionAttempt]) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let tx = client.transaction().await?;
        let mut inserted = 0usize;
        for row in rows {
            inserted += tx
                .execute(
                    r#"
                    INSERT INTO connection_attempts (
                        id, report_id, subscription_id, support_tag, app_version, platform,
                        address, port, protocol, outcome, detail, waited_ms,
                        client_country, client_org, ip_hash, attempted_at
                    ) VALUES (
                        $1,$2,$3,$4,$5,$6,$7::inet,$8,$9,$10,$11,$12,$13::text::char(2),$14,$15,$16
                    )
                    ON CONFLICT (id) DO UPDATE SET
                        outcome     = EXCLUDED.outcome,
                        detail      = EXCLUDED.detail,
                        waited_ms   = EXCLUDED.waited_ms,
                        attempted_at = EXCLUDED.attempted_at
                    "#,
                    &[
                        &row.id,
                        &row.report_id,
                        &row.subscription_id,
                        &row.support_tag,
                        &row.app_version,
                        &row.platform,
                        &row.address,
                        &row.port,
                        &row.protocol,
                        &row.outcome,
                        &row.detail,
                        &row.waited_ms,
                        &row.client_country,
                        &row.client_org,
                        &row.ip_hash,
                        &row.attempted_at,
                    ],
                )
                .await? as usize;
        }
        tx.commit().await?;
        Ok(inserted)
    }

    /// Наблюдения нового формата: upsert по id, без раннего выхода по
    /// `batch_id`.
    ///
    /// `insert_batch` намеренно пропускает весь батч, если он уже встречался:
    /// это защита старых клиентов от повторной отправки. Но при upsert-логике
    /// отчёта повтор может принести **больше** таргетов (первая попытка
    /// оборвалась), и пропускать их нельзя — иначе отчёт обновился, а
    /// наблюдения остались от неполной версии.
    pub async fn upsert_observations(&self, rows: &[Observation]) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let tx = client.transaction().await?;
        let mut written = 0usize;
        for row in rows {
            written += tx
                .execute(
                    r#"
                    INSERT INTO block_observations (
                        id, subscription_id, source, batch_id, support_tag, app_version,
                        platform, os, target_host, address, node_id, node_host, port,
                        protocol, transport, state, rtt_ms, icmp_ok, probe, session,
                        country_code, asn, client_country, client_org, client_city,
                        ip_prefix, ip_hash, observed_at,
                        app, device, network, target_country, error, note
                    ) VALUES (
                        $1,$2,$3,$4,$5,$6,$7,$8,$9,$10::inet,$11,$12,$13,$14,$15,$16,$17,$18,
                        $19,$20,$21::text::char(2),$22,$23,$24,$25,$26::text::cidr,$27,
                        $28,$29,$30,$31,$32,$33,$34
                    )
                    ON CONFLICT (id) DO UPDATE SET
                        state       = EXCLUDED.state,
                        rtt_ms      = EXCLUDED.rtt_ms,
                        transport   = EXCLUDED.transport,
                        probe       = EXCLUDED.probe,
                        session     = EXCLUDED.session,
                        observed_at = EXCLUDED.observed_at
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
                        &row.client_org,
                        &row.client_city,
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
                .await? as usize;
        }
        tx.commit().await?;
        Ok(written)
    }

    /// Отчёты для поддержки: по `support_tag`, подписке или просто последние.
    ///
    /// Отдаёт шапку отчёта и счётчик попыток; тело — отдельным полем, чтобы
    /// список оставался лёгким.
    pub async fn list_client_reports(
        &self,
        support_tag: Option<&str>,
        subscription_id: Option<uuid::Uuid>,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                r#"
                SELECT r.id, r.subscription_id, r.support_tag, r.mode,
                       r.app_version, r.platform, r.os, r.observed_at,
                       r.client_country, r.client_org, r.client_city,
                       r.config, r.mtu, r.tunnel, r.network, r.session, r.payload,
                       (SELECT count(*) FROM connection_attempts a WHERE a.report_id = r.id)
                           AS attempts_total,
                       (SELECT count(*) FROM connection_attempts a
                         WHERE a.report_id = r.id AND a.outcome = ANY($4)) AS attempts_failed
                FROM client_reports r
                WHERE ($1::text IS NULL OR r.support_tag = $1)
                  AND ($2::uuid IS NULL OR r.subscription_id = $2)
                ORDER BY r.created_at DESC
                LIMIT $3
                "#,
                &[
                    &support_tag,
                    &subscription_id,
                    &limit,
                    &FAIL_STATES
                        .iter()
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>(),
                ],
            )
            .await?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<_, uuid::Uuid>("id"),
                    "subscription_id": r.get::<_, Option<uuid::Uuid>>("subscription_id"),
                    "support_tag": r.get::<_, Option<String>>("support_tag"),
                    "mode": r.get::<_, Option<String>>("mode"),
                    "app_version": r.get::<_, String>("app_version"),
                    "platform": r.get::<_, String>("platform"),
                    "os": r.get::<_, Option<String>>("os"),
                    "observed_at": r.get::<_, DateTime<Utc>>("observed_at"),
                    "client_country": r.get::<_, Option<String>>("client_country"),
                    "client_org": r.get::<_, Option<String>>("client_org"),
                    "client_city": r.get::<_, Option<String>>("client_city"),
                    "config": r.get::<_, Option<serde_json::Value>>("config"),
                    "mtu": r.get::<_, Option<serde_json::Value>>("mtu"),
                    "tunnel": r.get::<_, Option<serde_json::Value>>("tunnel"),
                    "network": r.get::<_, Option<serde_json::Value>>("network"),
                    "session": r.get::<_, Option<serde_json::Value>>("session"),
                    "payload": r.get::<_, serde_json::Value>("payload"),
                    "attempts_total": r.get::<_, i64>("attempts_total"),
                    "attempts_failed": r.get::<_, i64>("attempts_failed"),
                })
            })
            .collect())
    }

    /// Попытки подключения по отчёту — «что именно не так».
    pub async fn list_attempts(&self, report_id: uuid::Uuid) -> Result<Vec<serde_json::Value>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let rows = client
            .query(
                r#"
                SELECT address, port, protocol, outcome, detail, waited_ms, attempted_at
                FROM connection_attempts
                WHERE report_id = $1
                ORDER BY attempted_at
                "#,
                &[&report_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "address": r.get::<_, Option<IpAddr>>("address"),
                    "port": r.get::<_, Option<i32>>("port"),
                    "protocol": r.get::<_, Option<String>>("protocol"),
                    "outcome": r.get::<_, Option<String>>("outcome"),
                    "detail": r.get::<_, Option<String>>("detail"),
                    "waited_ms": r.get::<_, Option<i32>>("waited_ms"),
                    "attempted_at": r.get::<_, DateTime<Utc>>("attempted_at"),
                })
            })
            .collect())
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
                       asn, client_country, state, transport, icmp_ok, ip_hash, created_at
                FROM block_observations
                WHERE created_at BETWEEN $1 AND $2
                  AND ($3::inet IS NULL OR address = $3)
            )
            SELECT ({keys})::text AS group_key,
                   count(*)::BIGINT AS observations,
                   count(DISTINCT asn)::BIGINT AS distinct_asn,
                   count(DISTINCT ip_hash)::BIGINT AS distinct_clients,
                   count(*) FILTER (
                       WHERE state = ANY($5)
                          OR (state = ANY($7) AND lower(coalesce(transport, 'tcp')) = 'tcp')
                   )::BIGINT AS fail_count,
                   count(*) FILTER (WHERE state = ANY($6))::BIGINT AS ok_count,
                   count(*) FILTER (
                       WHERE state = ANY($7) AND lower(coalesce(transport, 'udp')) <> 'tcp'
                   )::BIGINT AS inconclusive_count,
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
        let inconclusive: Vec<String> = INCONCLUSIVE_STATES.iter().map(|s| s.to_string()).collect();

        let rows = client
            .query(
                &sql,
                &[&from, &to, &address, &min_observations, &fail, &ok, &inconclusive],
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
                let verdict = if distinct_asn >= 3 && fail_count >= 5 && ratio >= 0.8 {
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
                    "inconclusive": r.get::<_, i64>("inconclusive_count"),
                    "icmp_fail": r.get::<_, i64>("icmp_fail"),
                    "first_at": r.get::<_, DateTime<Utc>>("first_at"),
                    "last_at": r.get::<_, DateTime<Utc>>("last_at"),
                    "fail_ratio": (ratio * 100.0).round() / 100.0,
                    "judged": judged,
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

#[cfg(test)]
mod sql_shape_tests {
    use super::INSERT_OBSERVATION_SQL;

    /// Число колонок, число `$N` и непрерывность нумерации должны совпадать:
    /// иначе Postgres отвечает ошибкой уже в проде, а не в тестах.
    #[test]
    fn insert_observation_sql_columns_match_params() {
        let cols_part = INSERT_OBSERVATION_SQL
            .split("INSERT INTO block_observations (")
            .nth(1)
            .expect("список колонок")
            .split(')')
            .next()
            .expect("конец списка колонок");
        let cols = cols_part.split(',').filter(|c| !c.trim().is_empty()).count();

        // `$N` встречаются только в списке значений, поэтому берём весь запрос.
        let mut nums: Vec<u32> = Vec::new();
        let mut rest = INSERT_OBSERVATION_SQL;
        while let Some(pos) = rest.find('$') {
            rest = &rest[pos + 1..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u32>() {
                nums.push(n);
            }
        }
        nums.sort_unstable();
        nums.dedup();

        assert_eq!(cols, nums.len(), "колонок {cols}, параметров {}", nums.len());
        assert_eq!(
            nums,
            (1..=nums.len() as u32).collect::<Vec<_>>(),
            "нумерация параметров должна быть непрерывной"
        );
    }
}

#[cfg(test)]
mod sql_cast_tests {
    use super::INSERT_OBSERVATION_SQL;

    /// Каст `::text::cidr` / `::text::char(2)` должен стоять именно на колонке
    /// этого типа. При вставке новой колонки в середину списка номер сдвигается,
    /// а каст остаётся на старом месте — Postgres тогда отвечает
    /// `error serializing parameter N` и каждый отчёт получает 500.
    #[test]
    fn casts_match_column_types() {
        let cols_part = INSERT_OBSERVATION_SQL
            .split("INSERT INTO block_observations (")
            .nth(1)
            .expect("колонки")
            .split(')')
            .next()
            .expect("конец колонок");
        let cols: Vec<&str> = cols_part
            .split(',')
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .collect();

        let values_part = INSERT_OBSERVATION_SQL
            .split(") VALUES (")
            .nth(1)
            .expect("значения");

        // Собираем «номер параметра -> каст», идущий сразу за ним.
        let mut casts: Vec<(usize, &str)> = Vec::new();
        let mut rest = values_part;
        let mut offset = 0usize;
        while let Some(pos) = rest.find('$') {
            let abs = offset + pos;
            rest = &rest[pos + 1..];
            offset = abs + 1;
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            let Ok(num) = digits.parse::<usize>() else {
                continue;
            };
            let after = &rest[digits.len()..];
            let after = after.trim_start();
            if let Some(cast) = after.strip_prefix("::text::") {
                let name: String = cast.chars().take_while(|c| c.is_alphanumeric() || *c == '(' || *c == ')').collect();
                casts.push((num, Box::leak(name.into_boxed_str())));
            }
        }

        for (num, cast) in casts {
            let col = cols
                .get(num - 1)
                .unwrap_or_else(|| panic!("параметр ${num} вне списка колонок"));
            let ok = match cast {
                "cidr" => col.ends_with("_prefix") || *col == "address",
                "char(2)" => col.ends_with("country") || col.ends_with("country_code"),
                other => panic!("неизвестный каст {other} на колонке {col}"),
            };
            assert!(ok, "каст ::{cast} стоит на колонке {col} (параметр ${num})");
        }
    }
}
