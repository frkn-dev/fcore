//! Телеметрия блокировок: `/v1/report/block` и `/v1/report/scan`.
//!
//! Контракт снят с клиента 4.8.14.75 (`frkn-docs/block-telemetry-client-contract.md`):
//! оба эндпойнта ходят через шифрованный конверт gateway (`with_agw_decryption` +
//! `encrypt_gateway_reply`), payload — `{auth_data, report_id|scan_id, report|scan}`.
//!
//! Что клиент не шлёт и что нельзя требовать: `error`, `device`, `note`,
//! `probe.icmp.rtt_ms`, `scan.results[].tls`, `dns_ok`. `target.address` бывает
//! пустым (DNS не ответил) — это валидный и самый информативный отчёт.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sha2::{Digest, Sha256};
use serde::Deserialize;
use serde_json::Value;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use warp::{Rejection, Reply};

use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, NodeStorageOperations,
    Subscription, SubscriptionOperations,
};

use crate::postgres::report::{Observation, FAIL_STATES, OK_STATES};
use crate::sync::MemSync;

use super::admin::check_token;
use super::amnezia::extract_subscription_id;
use super::share::{auth_data_has_share_token, client_ip, RateLimiter};

/// Лимит тела одиночного отчёта.
///
/// 64 КБ, а не 16: с 4.8.14.79 отчёт — это пачка (`targets[]` + `attempts[]`),
/// и десять таргетов с попытками уже не влезают в прежние 16 КБ. Старый
/// одиночный отчёт занимает единицы килобайт, так что запас ничего не ломает.
pub const MAX_REPORT_BODY_BYTES: u64 = 64 * 1024;
/// Лимит тела среза «Check all servers».
pub const MAX_SCAN_BODY_BYTES: u64 = 128 * 1024;
/// Допустимое опережение часов клиента.
const MAX_CLOCK_SKEW_SECS: i64 = 300;
/// Окно дедупа «тот же юзер про ту же цель».
pub const DEDUP_WINDOW_SECS: i64 = 600;
/// Сколько записей в срезе принимаем (клиент режет на 512).
pub const MAX_SCAN_RESULTS: usize = 512;

// ---------------------------------------------------------------------------
// Payload
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct BlockReportEnvelope {
    #[serde(default)]
    pub auth_data: Value,
    pub report_id: String,
    pub report: ReportBody,
}

#[derive(Debug, Deserialize)]
pub struct ReportBody {
    #[serde(default)]
    pub app: Option<Value>,
    #[serde(default)]
    pub support_tag: Option<String>,
    pub target: ReportTarget,
    #[serde(default)]
    pub probe: Option<Value>,
    #[serde(default)]
    pub session: Option<Value>,
    /// Опционален: на iOS/macOS/Windows его нет, на Android carrier и
    /// network_country бывают пустыми строками.
    #[serde(default)]
    pub network: Option<Value>,
    #[serde(default)]
    pub at: Option<String>,
    #[serde(default)]
    pub error: Option<Value>,
    #[serde(default)]
    pub device: Option<Value>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReportTarget {
    #[serde(default)]
    pub host: Option<String>,
    /// Может быть пустой строкой, если DNS не ответил.
    #[serde(default)]
    pub address: Option<String>,
    pub protocol: String,
    pub port: i64,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub country_code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BlockScanEnvelope {
    #[serde(default)]
    pub auth_data: Value,
    pub scan_id: String,
    pub scan: ScanBody,
}

#[derive(Debug, Deserialize)]
pub struct ScanBody {
    #[serde(default)]
    pub app: Option<Value>,
    #[serde(default)]
    pub support_tag: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub network: Option<Value>,
    #[serde(default)]
    pub device: Option<Value>,
    #[serde(default)]
    pub results: Vec<ScanResult>,
}

/// В записи среза есть **либо** `tcp`, **либо** `udp`: транспорт выбран по
/// протоколу. `skipped` — «не успели/отменили», не ошибка сети.
#[derive(Debug, Deserialize)]
pub struct ScanResult {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    pub port: i64,
    pub protocol: String,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub tcp: Option<String>,
    #[serde(default)]
    pub udp: Option<String>,
    #[serde(default)]
    pub rtt_ms: Option<i64>,
}

// ---------------------------------------------------------------------------
// Лимитеры
// ---------------------------------------------------------------------------

/// Лимит по подписке (ключ — subscription uuid).
pub struct SubRateLimiter {
    limit: usize,
    window: std::time::Duration,
    hits: std::sync::Mutex<std::collections::HashMap<uuid::Uuid, Vec<std::time::Instant>>>,
}

impl SubRateLimiter {
    pub fn new(limit: usize, window: std::time::Duration) -> Self {
        Self {
            limit,
            window,
            hits: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub fn check(&self, key: uuid::Uuid) -> bool {
        let now = std::time::Instant::now();
        let mut map = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        let entries = map.entry(key).or_default();
        entries.retain(|t| now.duration_since(*t) < self.window);
        if entries.len() >= self.limit {
            return false;
        }
        entries.push(now);
        true
    }
}

// ---------------------------------------------------------------------------
// Хелперы
// ---------------------------------------------------------------------------

fn json_status(code: u16, message: &str) -> warp::reply::Response {
    let status =
        warp::http::StatusCode::from_u16(code).unwrap_or(warp::http::StatusCode::BAD_REQUEST);
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({ "status": code, "message": message })),
        status,
    )
    .into_response()
}

/// Ответ об успехе: клиент смотрит только на отсутствие `status >= 400`.
fn accepted(body: Value) -> warp::reply::Response {
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({
            "status": 202,
            "response": body,
        })),
        warp::http::StatusCode::ACCEPTED,
    )
    .into_response()
}

pub(crate) fn batch_uuid(raw: &str, sub_id: Option<uuid::Uuid>) -> uuid::Uuid {
    if let Ok(id) = uuid::Uuid::parse_str(raw) {
        return id;
    }
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    if let Some(sub) = sub_id {
        hasher.update(sub.as_bytes());
    }
    hasher.update(raw.as_bytes());
    let hash = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    uuid::Uuid::from_bytes(bytes)
}

fn observation_uuid(
    batch: uuid::Uuid,
    index: usize,
    address: Option<IpAddr>,
    host: Option<&str>,
) -> uuid::Uuid {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(batch.as_bytes());
    hasher.update(index.to_le_bytes());
    match address {
        Some(a) => hasher.update(a.to_string().as_bytes()),
        None => hasher.update(host.unwrap_or("").as_bytes()),
    }
    let hash = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    uuid::Uuid::from_bytes(bytes)
}

fn ip_prefix(ip: IpAddr) -> Option<String> {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            Some(format!("{}.{}.{}.0/24", o[0], o[1], o[2]))
        }
        IpAddr::V6(v6) => {
            let mut seg = v6.segments();
            for s in seg.iter_mut().skip(3) {
                *s = 0;
            }
            Some(format!("{}/48", std::net::Ipv6Addr::from(seg)))
        }
    }
}

fn ip_hash(ip: IpAddr, salt: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(ip.to_string().as_bytes());
    hasher.finalize()[..16].to_vec()
}

fn trimmed(s: &str, max: usize) -> Option<String> {
    let t = s.trim();
    if t.is_empty() || t.chars().count() > max {
        None
    } else {
        Some(t.to_string())
    }
}

fn opt_trimmed(s: &Option<String>, max: usize) -> Option<String> {
    s.as_deref().and_then(|v| trimmed(v, max))
}

fn parse_address(raw: &Option<String>) -> Result<Option<IpAddr>, String> {
    match raw.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(s) => s
            .parse::<IpAddr>()
            .map(Some)
            .map_err(|_| "invalid_address".into()),
    }
}

fn parse_time(raw: &Option<String>) -> Result<Option<DateTime<Utc>>, String> {
    match raw.as_deref() {
        None => Ok(None),
        Some(s) => {
            let dt = DateTime::parse_from_rfc3339(s)
                .map_err(|_| "invalid_at".to_string())?
                .with_timezone(&Utc);
            if dt > Utc::now() + ChronoDuration::seconds(MAX_CLOCK_SKEW_SECS) {
                return Err("at_in_future".to_string());
            }
            Ok(Some(dt))
        }
    }
}

fn app_fields(app: &Option<Value>) -> (String, String, Option<String>) {
    let app = app.as_ref();
    let version = app
        .and_then(|a| a.get("version"))
        .and_then(|v| v.as_str())
        .and_then(|v| trimmed(v, 32))
        .unwrap_or_else(|| "unknown".to_string());
    let platform = app
        .and_then(|a| a.get("platform"))
        .and_then(|v| v.as_str())
        .and_then(|v| trimmed(v, 32))
        .unwrap_or_else(|| "unknown".to_string());
    let os = app
        .and_then(|a| a.get("os"))
        .and_then(|v| v.as_str())
        .and_then(|v| trimmed(v, 64));
    (version, platform, os)
}

/// Первое не-`skipped`/не-`answered` состояние из `probe.tcp[]` / `probe.udp[]`.
fn report_state(probe: &Option<Value>) -> Option<String> {
    let probe = probe.as_ref()?;
    for key in ["udp", "tcp"] {
        if let Some(arr) = probe.get(key).and_then(|v| v.as_array()) {
            for entry in arr {
                if let Some(state) = entry.get("state").and_then(|v| v.as_str()) {
                    return Some(state.to_string());
                }
            }
        }
    }
    None
}

fn report_transport(probe: &Option<Value>) -> Option<String> {
    let probe = probe.as_ref()?;
    if probe.get("udp").and_then(|v| v.as_array()).is_some() {
        return Some("udp".to_string());
    }
    if probe.get("tcp").and_then(|v| v.as_array()).is_some() {
        return Some("tcp".to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// POST /v1/report/block
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
// ---------------------------------------------------------------------------
// Новый формат отчёта (клиент 4.8.14.79+)
// ---------------------------------------------------------------------------
//
// Клиент перестал слать один `target` и присылает отчёт целиком:
//
//   tunnel off (проверка серверов): support_tag, config, mtu, targets[], attempts[]
//   tunnel on  (проверка соединения): mode="tunnel", target, tunnel, session, mtu
//
// `report_id` **обязателен в обоих форматах**: он и есть идемпотентность.
// Выдумывать его на сервере (хеш от тела) нельзя — тогда повторная отправка
// после правки одного поля становится новым отчётом, а настоящий повтор с тем
// же id мы всё равно не отличим. Клиент обязан прислать id один раз на отчёт и
// повторять его при переотправке.
//
// `/v1/report/block` принимает оба формата: старый (`report`-объект) уходит в
// `gateway_block_report_handler` без изменений, новый — в `parse_rich_report`.
// Различаем по наличию `report`, а не `report_id`: id есть у обоих.

/// Режим отчёта, если клиент его прислал.
fn report_mode(body: &Value) -> Option<String> {
    body.get("mode")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(24).collect())
}

/// Один таргет из `targets[]` (проверка доступных серверов).
#[derive(Debug, Deserialize)]
struct TargetEntry {
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    port: Option<i64>,
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    ms: Option<i64>,
    #[serde(default)]
    disabled_by_user: bool,
    #[serde(default)]
    in_use: bool,
}

/// Одна попытка из `attempts[]`.
#[derive(Debug, Deserialize)]
struct AttemptEntry {
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    port: Option<i64>,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    waited_ms: Option<i64>,
}

/// Разбирает новый отчёт: отчёт целиком + наблюдения по таргетам + попытки.
///
/// Возвращает `Err(сообщение)` только если тело совсем не годится (нет ни
/// таргетов, ни цели) — остальное деградирует мягко: битая запись
/// отбрасывается, ответ остаётся 202.
/// Разобранный отчёт: ровно то, что уйдёт в базу.
///
/// Чистая часть приёма — без БД. Поэтому её видно в тесте и можно напечатать:
/// «что именно мы сохранили из этого тела» перестаёт быть догадкой.
#[derive(Debug)]
struct RichParsed {
    report: crate::postgres::report::ClientReport,
    observations: Vec<Observation>,
    attempts: Vec<crate::postgres::report::ConnectionAttempt>,
    targets_seen: usize,
    targets_rejected: usize,
}

/// Детерминированный uuid для строки журнала попыток.
fn attempt_id(report_id: uuid::Uuid, index: usize, at: Option<&str>) -> uuid::Uuid {
    let mut hasher = Sha256::new();
    hasher.update(report_id.as_bytes());
    hasher.update(index.to_string().as_bytes());
    hasher.update(at.unwrap_or_default().as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes)
}

fn parse_rich_report(
    body: &Value,
    sub_id: Option<uuid::Uuid>,
    ip: IpAddr,
) -> Result<RichParsed, String> {
    let support_tag = body
        .get("support_tag")
        .and_then(|v| v.as_str())
        .and_then(|v| trimmed(v, 64));
    let at_raw = body.get("at").and_then(|v| v.as_str());
    let observed_at = parse_time(&at_raw.map(|s| s.to_string()))
        .ok()
        .flatten()
        .unwrap_or_else(Utc::now);

    let app_value = body.get("app").cloned();
    let (app_version, platform, os) = app_fields(&app_value);
    let geo = fcore::geoip::lookup_full(ip);

    // `report_id` обязателен: без него нет идемпотентности, а выдуманный на
    // сервере id не отличит повтор от нового отчёта.
    let report_id = body
        .get("report_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing_report_id".to_string())
        .and_then(|raw| {
            uuid::Uuid::parse_str(raw.trim()).map_err(|_| "invalid_report_id".to_string())
        })?;

    let targets: Vec<TargetEntry> = body
        .get("targets")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let attempts: Vec<AttemptEntry> = body
        .get("attempts")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let has_target = body.get("target").map(|v| !v.is_null()).unwrap_or(false);

    if targets.is_empty() && !has_target && attempts.is_empty() {
        return Err("empty_report".to_string());
    }

    let report = crate::postgres::report::ClientReport {
        id: report_id,
        subscription_id: sub_id,
        support_tag: support_tag.clone(),
        mode: report_mode(body),
        app_version: app_version.clone(),
        platform: platform.clone(),
        os: os.clone(),
        app: body.get("app").cloned().unwrap_or(Value::Null),
        config: body.get("config").cloned().unwrap_or(Value::Null),
        mtu: body.get("mtu").cloned().unwrap_or(Value::Null),
        tunnel: body.get("tunnel").cloned().unwrap_or(Value::Null),
        network: body.get("network").cloned().unwrap_or(Value::Null),
        session: body.get("session").cloned().unwrap_or(Value::Null),
        device: body.get("device").cloned().unwrap_or(Value::Null),
        payload: body.clone(),
        country_code: None,
        asn: geo.asn.map(|v| v as i32),
        client_country: geo.country.clone(),
        client_org: geo.org.clone(),
        client_city: geo.city.clone(),
        ip_prefix: ip_prefix(ip),
        ip_hash: Some(ip_hash(ip, b"frkn-block-telemetry-v1")),
        observed_at,
    };

    let mut observations: Vec<Observation> = Vec::new();
    let mut rejected = 0usize;

    let mut push_row = |address: Option<IpAddr>,
                        host: Option<String>,
                        port: i64,
                        protocol: String,
                        transport: Option<String>,
                        state: Option<String>,
                        rtt_ms: Option<i64>,
                        probe: Value,
                        source: &str,
                        index: usize| {
        observations.push(Observation {
            id: observation_uuid(report_id, index, address, host.as_deref()),
            subscription_id: sub_id,
            source: source.to_string(),
            batch_id: report_id,
            support_tag: support_tag.clone(),
            app_version: app_version.clone(),
            platform: platform.clone(),
            os: os.clone(),
            target_host: host,
            address,
            node_id: None,
            node_host: None,
            port: port as i32,
            protocol,
            transport,
            state,
            rtt_ms: rtt_ms.map(|v| v as i32),
            icmp_ok: None,
            target_country: None,
            error: Value::Null,
            note: None,
            app: body.get("app").cloned().unwrap_or(Value::Null),
            device: body.get("device").cloned().unwrap_or(Value::Null),
            network: body.get("network").cloned().unwrap_or(Value::Null),
            probe,
            session: body.get("session").cloned().unwrap_or(Value::Null),
            country_code: None,
            asn: geo.asn.map(|v| v as i32),
            client_country: geo.country.clone(),
            client_org: geo.org.clone(),
            client_city: geo.city.clone(),
            ip_prefix: ip_prefix(ip),
            ip_hash: Some(ip_hash(ip, b"frkn-block-telemetry-v1")),
            observed_at,
        });
    };

    for (index, entry) in targets.iter().enumerate() {
        let address = match parse_address(&entry.address) {
            Ok(v) => v,
            Err(_) => {
                rejected += 1;
                continue;
            }
        };
        let Some(port) = entry.port.filter(|p| (1..=65535).contains(p)) else {
            rejected += 1;
            continue;
        };
        let transport = entry
            .transport
            .as_deref()
            .and_then(|v| trimmed(v, 8))
            .map(|v| v.to_ascii_lowercase());
        let protocol = transport.clone().unwrap_or_else(|| "unknown".to_string());
        push_row(
            address,
            None,
            port,
            protocol,
            transport,
            entry.state.as_deref().and_then(|v| trimmed(v, 24)),
            entry.ms,
            serde_json::json!({
                "disabled_by_user": entry.disabled_by_user,
                "in_use": entry.in_use,
            }),
            "targets",
            index,
        );
    }

    // Режим tunnel: цель одна, проб нет — но «куда подключались» записать надо.
    if let Some(target) = body.get("target").filter(|v| !v.is_null()) {
        let address = target
            .get("address")
            .and_then(|v| v.as_str())
            .or_else(|| target.get("host").and_then(|v| v.as_str()));
        let port = target.get("port").and_then(|v| v.as_i64());
        if let (Ok(addr), Some(port)) = (parse_address(&address.map(|s| s.to_string())), port) {
            if (1..=65535).contains(&port) {
                let protocol = target
                    .get("protocol")
                    .and_then(|v| v.as_str())
                    .and_then(|v| trimmed(v, 32))
                    .unwrap_or_else(|| "unknown".to_string());
                push_row(
                    addr,
                    target
                        .get("host")
                        .and_then(|v| v.as_str())
                        .and_then(|v| trimmed(v, 255)),
                    port,
                    protocol,
                    None,
                    None,
                    None,
                    Value::Null,
                    "tunnel",
                    targets.len(),
                );
            }
        }
    }

    let mut attempt_rows: Vec<crate::postgres::report::ConnectionAttempt> = Vec::new();
    for (index, entry) in attempts.iter().enumerate() {
        let address = parse_address(&entry.address).ok().flatten();
        let port = entry
            .port
            .filter(|p| (1..=65535).contains(p))
            .map(|p| p as i32);
        let attempted_at = parse_time(&entry.at).ok().flatten().unwrap_or(observed_at);
        attempt_rows.push(crate::postgres::report::ConnectionAttempt {
            id: attempt_id(report_id, index, entry.at.as_deref()),
            report_id,
            subscription_id: sub_id,
            support_tag: support_tag.clone(),
            app_version: app_version.clone(),
            platform: platform.clone(),
            address,
            port,
            protocol: entry
                .protocol
                .as_deref()
                .and_then(|v| trimmed(v, 32))
                .map(|v| v.to_ascii_lowercase()),
            outcome: entry
                .outcome
                .as_deref()
                .and_then(|v| trimmed(v, 24))
                .map(|v| v.to_ascii_lowercase()),
            detail: entry.detail.as_deref().and_then(|v| trimmed(v, 200)),
            waited_ms: entry.waited_ms.map(|v| v.min(i32::MAX as i64) as i32),
            client_country: geo.country.clone(),
            client_org: geo.org.clone(),
            ip_hash: Some(ip_hash(ip, b"frkn-block-telemetry-v1")),
            attempted_at,
        });
    }

    Ok(RichParsed {
        report,
        observations,
        attempts: attempt_rows,
        targets_seen: targets.len(),
        targets_rejected: rejected,
    })
}

async fn store_rich_report<N, C, S>(
    body: &Value,
    sub_id: Option<uuid::Uuid>,
    memory: &MemSync<N, C, S>,
    ip: IpAddr,
) -> Result<serde_json::Value, String>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations + ConnectionBaseOperations + Sync + Send + Clone + 'static,
    S: SubscriptionOperations + Send + Sync + Clone + 'static,
{
    let parsed = parse_rich_report(body, sub_id, ip)?;
    let report_id = parsed.report.id;

    let stored = memory
        .db
        .report()
        .insert_client_report(&parsed.report)
        .await
        .map_err(|e| format!("db: {e}"))?;

    if !parsed.observations.is_empty() {
        // upsert, а не insert_batch: повтор может принести больше таргетов
        // (первая отправка оборвалась), и наблюдения должны обновиться.
        if let Err(e) = memory
            .db
            .report()
            .upsert_observations(&parsed.observations)
            .await
        {
            tracing::warn!("rich report observations upsert failed: {e}");
        }
    }

    let attempts_stored = memory
        .db
        .report()
        .insert_attempts(&parsed.attempts)
        .await
        .map_err(|e| format!("db: {e}"))?;

    Ok(serde_json::json!({
        "id": report_id,
        // stored = строка создана впервые; false = обновлена повторной отправкой
        "created": stored,
        "dedup": !stored,
        "mode": parsed.report.mode,
        "targets_count": parsed.targets_seen,
        "targets_rejected": parsed.targets_rejected,
        "attempts_count": attempts_stored,
    }))
}

/// Единая точка входа `/v1/report/block`: различает старый и новый формат.
/// Отказ клиенту + запись причины в журнал.
///
/// Без этого 400 уходил молча: в nginx видно «400, 48 байт», а что именно
/// не понравилось — только в теле ответа, которого у нас нет. Пишем статус,
/// причину и набор ключей запроса (без значений: в них api_key и support_tag).
fn reject_report(status: u16, reason: &str, body: &Value) -> warp::reply::Response {
    let keys: Vec<String> = body
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let app = body
        .get("app")
        .and_then(|a| a.get("version"))
        .and_then(|v| v.as_str())
        .unwrap_or("—");
    tracing::warn!(
        event = "report_rejected",
        status,
        reason,
        app,
        keys = ?keys,
        "отчёт отклонён"
    );
    json_status(status, reason)
}

pub async fn gateway_report_handler<N, C, S>(
    body: Value,
    memory: MemSync<N, C, S>,
    remote: Option<SocketAddr>,
    x_forwarded_for: Option<String>,
    ip_limiter: Arc<RateLimiter>,
    sub_limiter: Arc<SubRateLimiter>,
) -> Result<warp::reply::Response, Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    // Что реально пришло: без этой строки не видно, какую ветку выбрал
    // диспетчер и почему «202, а в базе пусто». Пишем только имена ключей
    // и версию приложения — значения содержат api_key и support_tag.
    {
        let keys: Vec<String> = body
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        tracing::info!(
            event = "report_received",
            keys = ?keys,
            has_report = body.get("report").map(|v| v.is_object()).unwrap_or(false),
            has_scan = body.get("scan").map(|v| v.is_object()).unwrap_or(false),
            targets = body.get("targets").and_then(|v| v.as_array()).map(|a| a.len()),
            attempts = body.get("attempts").and_then(|v| v.as_array()).map(|a| a.len()),
            mode = body.get("mode").and_then(|m| m.as_str()),
            app = body.get("app").and_then(|a| a.get("version")).and_then(|v| v.as_str()),
            "отчёт получен"
        );
    }

    // Старые форматы узнаём по вложенному объекту: `report` — одиночный
    // отчёт, `scan` — срез «Check all servers». Идём прежними путями, чтобы
    // поведение старых клиентов не менялось ни на байт. `report_id` есть в
    // обоих семействах, поэтому различать по нему нельзя.
    if body.get("report").map(|v| v.is_object()).unwrap_or(false) {
        let envelope: BlockReportEnvelope = match serde_json::from_value(body.clone()) {
            Ok(v) => v,
            Err(e) => return Ok(reject_report(400, &format!("invalid_report: {e}"), &body)),
        };
        return gateway_block_report_handler(
            envelope,
            memory,
            remote,
            x_forwarded_for,
            ip_limiter,
            sub_limiter,
        )
        .await;
    }

    // Клиент 4.8.14.79+ шлёт новый формат и в `/report/scan` тоже (report_id
    // приходит в обоих режимах), поэтому этот же диспетчер стоит на обоих
    // маршрутах. Старый срез по-прежнему уходит своим путём.
    if body.get("scan").map(|v| v.is_object()).unwrap_or(false) {
        let envelope: BlockScanEnvelope = match serde_json::from_value(body.clone()) {
            Ok(v) => v,
            Err(e) => return Ok(reject_report(400, &format!("invalid_scan: {e}"), &body)),
        };
        return gateway_scan_report_handler(
            envelope,
            memory,
            remote,
            x_forwarded_for,
            ip_limiter,
            sub_limiter,
        )
        .await;
    }

    let ip = client_ip(remote, x_forwarded_for.as_deref());
    if !ip_limiter.check(ip) {
        return Ok(reject_report(429, "too_many_requests", &body));
    }

    let auth = body.get("auth_data").cloned().unwrap_or(Value::Null);
    let sub_id = extract_subscription_id(&auth);
    if sub_id.is_none() && !auth_data_has_share_token(&auth) {
        return Ok(reject_report(401, "unauthorized", &body));
    }
    if let Some(sub) = sub_id {
        if !sub_limiter.check(sub) {
            return Ok(reject_report(429, "too_many_requests_subscription", &body));
        }
    }

    match store_rich_report(&body, sub_id, &memory, ip).await {
        Ok(result) => {
            tracing::info!(
                event = "report_stored",
                report_id = %result
                    .get("report_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("—"),
                created = result.get("created").and_then(|v| v.as_bool()).unwrap_or(false),
                targets = body
                    .get("targets")
                    .and_then(|t| t.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0),
                attempts = body
                    .get("attempts")
                    .and_then(|t| t.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0),
                mode = body.get("mode").and_then(|m| m.as_str()).unwrap_or("—"),
                app = body
                    .get("app")
                    .and_then(|a| a.get("version"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("—"),
                "отчёт сохранён"
            );
            Ok(accepted(result))
        }
        // Отдельные 400 с внятным текстом: клиент должен видеть, что именно
        // не так, а не общий «bad request».
        Err(reason)
            if matches!(
                reason.as_str(),
                "empty_report" | "missing_report_id" | "invalid_report_id"
            ) =>
        {
            Ok(reject_report(400, &reason, &body))
        }
        Err(reason) => {
            tracing::error!("rich report failed: {reason}");
            Ok(json_status(500, "internal"))
        }
    }
}

pub async fn gateway_block_report_handler<N, C, S>(
    req: BlockReportEnvelope,
    memory: MemSync<N, C, S>,
    remote: Option<SocketAddr>,
    x_forwarded_for: Option<String>,
    ip_limiter: Arc<RateLimiter>,
    sub_limiter: Arc<SubRateLimiter>,
) -> Result<warp::reply::Response, Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let ip = client_ip(remote, x_forwarded_for.as_deref());
    if !ip_limiter.check(ip) {
        return Ok(json_status(429, "too_many_requests"));
    }

    let sub_id = extract_subscription_id(&req.auth_data);
    if sub_id.is_none() && !auth_data_has_share_token(&req.auth_data) {
        return Ok(json_status(401, "unauthorized"));
    }
    if let Some(sub) = sub_id {
        if !sub_limiter.check(sub) {
            return Ok(json_status(429, "too_many_requests_subscription"));
        }
    }

    let target = &req.report.target;
    if target.port < 1 || target.port > 65535 {
        return Ok(json_status(400, "invalid_port"));
    }
    let protocol = match trimmed(&target.protocol, 32) {
        Some(v) => v,
        None => return Ok(json_status(400, "invalid_protocol")),
    };
    let address = match parse_address(&target.address) {
        Ok(v) => v,
        Err(e) => return Ok(json_status(400, &e)),
    };
    let observed_at = match parse_time(&req.report.at) {
        Ok(Some(t)) => t,
        Ok(None) => Utc::now(),
        Err(e) => return Ok(json_status(400, &e)),
    };

    let host = opt_trimmed(&target.host, 255);
    let (node_id, node_host) = match (address, host.as_deref()) {
        (Some(addr), _) => memory
            .db
            .report()
            .resolve_node(addr)
            .await
            .unwrap_or((None, None)),
        (None, Some(h)) => memory
            .db
            .report()
            .resolve_node_by_host(h)
            .await
            .unwrap_or((None, None)),
        _ => (None, None),
    };

    let geo = fcore::geoip::lookup_full(ip);
    let (asn, client_country) = (geo.asn, geo.country.clone());
    let (app_version, platform, os) = app_fields(&req.report.app);
    let batch_id = batch_uuid(&req.report_id, sub_id);

    let row = Observation {
        id: observation_uuid(batch_id, 0, address, host.as_deref()),
        subscription_id: sub_id,
        source: "report".to_string(),
        batch_id,
        support_tag: opt_trimmed(&req.report.support_tag, 64),
        app_version,
        platform,
        os,
        target_host: host,
        address,
        node_id,
        node_host,
        port: target.port as i32,
        protocol,
        transport: report_transport(&req.report.probe),
        state: report_state(&req.report.probe),
        rtt_ms: req
            .report
            .probe
            .as_ref()
            .and_then(|p| p.get("icmp"))
            .and_then(|i| i.get("rtt_ms"))
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        icmp_ok: req
            .report
            .probe
            .as_ref()
            .and_then(|p| p.get("icmp"))
            .and_then(|i| i.get("ok"))
            .and_then(|v| v.as_bool()),
        target_country: opt_trimmed(&target.country, 64),
        error: req.report.error.clone().unwrap_or(Value::Null),
        note: opt_trimmed(&req.report.note, 500),
        app: req.report.app.clone().unwrap_or(Value::Null),
        device: req.report.device.clone().unwrap_or(Value::Null),
        network: req.report.network.clone().unwrap_or(Value::Null),
        probe: req.report.probe.clone().unwrap_or(Value::Null),
        session: req.report.session.clone().unwrap_or(Value::Null),
        country_code: opt_trimmed(&target.country_code, 2)
            .or_else(|| opt_trimmed(&target.country_code, 3)),
        asn: asn.map(|v| v as i32),
        client_country,
        client_org: geo.org.clone(),
        client_city: geo.city.clone(),
        ip_prefix: ip_prefix(ip),
        ip_hash: Some(ip_hash(ip, b"frkn-block-telemetry-v1")),
        observed_at,
    };

    if let Ok(count) = memory
        .db
        .report()
        .count_recent(
            row.address,
            row.target_host.as_deref(),
            row.port,
            sub_id,
            DEDUP_WINDOW_SECS,
        )
        .await
    {
        if count > 0 {
            return Ok(accepted(serde_json::json!({
                "id": row.id,
                "dedup": true,
                "reason": "window",
                "results_count": 1,
            })));
        }
    }

    match memory
        .db
        .report()
        .insert_batch(batch_id, std::slice::from_ref(&row))
        .await
    {
        Ok(true) => Ok(accepted(serde_json::json!({
            "id": row.id,
            "dedup": false,
            "results_count": 1,
        }))),
        Ok(false) => Ok(accepted(serde_json::json!({
            "id": row.id,
            "dedup": true,
            "reason": "report_id",
            "results_count": 1,
        }))),
        Err(e) => {
            tracing::error!("block report insert failed: {e}");
            Ok(json_status(500, "internal"))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /v1/report/scan
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn gateway_scan_report_handler<N, C, S>(
    req: BlockScanEnvelope,
    memory: MemSync<N, C, S>,
    remote: Option<SocketAddr>,
    x_forwarded_for: Option<String>,
    ip_limiter: Arc<RateLimiter>,
    sub_limiter: Arc<SubRateLimiter>,
) -> Result<warp::reply::Response, Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let ip = client_ip(remote, x_forwarded_for.as_deref());
    if !ip_limiter.check(ip) {
        return Ok(json_status(429, "too_many_requests"));
    }

    let sub_id = extract_subscription_id(&req.auth_data);
    if sub_id.is_none() && !auth_data_has_share_token(&req.auth_data) {
        return Ok(json_status(401, "unauthorized"));
    }
    if let Some(sub) = sub_id {
        if !sub_limiter.check(sub) {
            return Ok(json_status(429, "too_many_requests_subscription"));
        }
    }

    if req.scan.results.is_empty() {
        return Ok(json_status(400, "empty_results"));
    }
    if req.scan.results.len() > MAX_SCAN_RESULTS {
        return Ok(json_status(413, "too_many_results"));
    }

    let observed_at = match parse_time(&req.scan.started_at) {
        Ok(Some(t)) => t,
        Ok(None) => Utc::now(),
        Err(e) => return Ok(json_status(400, &e)),
    };

    let geo = fcore::geoip::lookup_full(ip);
    let (asn, client_country) = (geo.asn, geo.country.clone());
    let (app_version, platform, os) = app_fields(&req.scan.app);
    let batch_id = batch_uuid(&req.scan_id, sub_id);
    let support_tag = opt_trimmed(&req.scan.support_tag, 64);
    let network = req.scan.network.clone().unwrap_or(Value::Null);

    let mut rows: Vec<Observation> = Vec::with_capacity(req.scan.results.len());
    for (index, result) in req.scan.results.iter().enumerate() {
        if result.port < 1 || result.port > 65535 {
            return Ok(json_status(400, "invalid_port"));
        }
        let protocol = match trimmed(&result.protocol, 32) {
            Some(v) => v,
            None => return Ok(json_status(400, "invalid_protocol")),
        };
        let address = match parse_address(&result.address) {
            Ok(v) => v,
            Err(e) => return Ok(json_status(400, &e)),
        };
        let host = opt_trimmed(&result.host, 255);
        let state = result
            .udp
            .clone()
            .or_else(|| result.tcp.clone())
            .and_then(|v| trimmed(&v, 24));
        let (node_id, node_host) = match (address, host.as_deref()) {
            (Some(addr), _) => memory
                .db
                .report()
                .resolve_node(addr)
                .await
                .unwrap_or((None, None)),
            (None, Some(h)) => memory
                .db
                .report()
                .resolve_node_by_host(h)
                .await
                .unwrap_or((None, None)),
            _ => (None, None),
        };

        rows.push(Observation {
            id: observation_uuid(batch_id, index, address, host.as_deref()),
            subscription_id: sub_id,
            source: "scan".to_string(),
            batch_id,
            support_tag: support_tag.clone(),
            app_version: app_version.clone(),
            platform: platform.clone(),
            os: os.clone(),
            target_host: host,
            address,
            node_id,
            node_host,
            port: result.port as i32,
            protocol,
            transport: opt_trimmed(&result.transport, 8),
            state,
            rtt_ms: result.rtt_ms.map(|v| v as i32),
            icmp_ok: None,
            target_country: None,
            error: Value::Null,
            note: None,
            app: req.scan.app.clone().unwrap_or(Value::Null),
            device: req.scan.device.clone().unwrap_or(Value::Null),
            network: network.clone(),
            probe: Value::Null,
            session: serde_json::json!({
                "mode": req.scan.mode,
                "duration_ms": req.scan.duration_ms,
                "network": network,
            }),
            country_code: None,
            asn: asn.map(|v| v as i32),
            client_country: client_country.clone(),
            client_org: geo.org.clone(),
            client_city: geo.city.clone(),
            ip_prefix: ip_prefix(ip),
            ip_hash: Some(ip_hash(ip, b"frkn-block-telemetry-v1")),
            observed_at,
        });
    }

    let results_count = rows.len();
    match memory.db.report().insert_batch(batch_id, &rows).await {
        Ok(true) => Ok(accepted(serde_json::json!({
            "id": batch_id,
            "dedup": false,
            "results_count": results_count,
        }))),
        Ok(false) => Ok(accepted(serde_json::json!({
            "id": batch_id,
            "dedup": true,
            "reason": "scan_id",
            "results_count": results_count,
        }))),
        Err(e) => {
            tracing::error!("block scan insert failed: {e}");
            Ok(json_status(500, "internal"))
        }
    }
}

// ---------------------------------------------------------------------------
// GET /v1/admin/report/block
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct BlockReportAggregateQuery {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub group_by: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub min_observations: Option<i64>,
}

#[derive(serde::Deserialize, Debug, Default)]
pub struct ObservationsQuery {
    #[serde(default)]
    pub subscription: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// GET /v1/admin/report/reports — отчёты клиента: кто прислал и что не так.
#[derive(Debug, Deserialize)]
pub struct ClientReportsQuery {
    /// `support_tag` из приложения — то, что юзер называет в поддержке.
    #[serde(default)]
    pub support_tag: Option<String>,
    #[serde(default)]
    pub subscription: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Отчёты + попытки подключения по каждому.
///
/// Это ответ на вопрос поддержки «юзер говорит, не подключается»: видно, с
/// какого адреса, на какой порт, с каким исходом и сколько он ждал, плюс режим
/// (tunnel/probe), конфиг, MTU и сессия.
pub async fn admin_client_reports_handler<N, C, S>(
    query: ClientReportsQuery,
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
) -> Result<warp::reply::Response, Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if !admin_enabled {
        return Ok(json_status(404, "not found"));
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(json_status(401, "unauthorized"));
    }

    let subscription = match query.subscription.as_deref() {
        Some(raw) if !raw.trim().is_empty() => match raw.trim().parse::<uuid::Uuid>() {
            Ok(v) => Some(v),
            Err(_) => return Ok(json_status(400, "invalid_subscription")),
        },
        _ => None,
    };
    let support_tag = query
        .support_tag
        .as_deref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty());
    let limit = query.limit.unwrap_or(50).clamp(1, 500);

    let reports = match memory
        .db
        .report()
        .list_client_reports(support_tag, subscription, limit)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("list_client_reports failed: {e}");
            return Ok(json_status(500, "internal"));
        }
    };

    // Попытки подмешиваем к каждому отчёту: без них «что не так» не читается.
    let mut out = Vec::with_capacity(reports.len());
    for mut report in reports {
        let id = report
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|v| uuid::Uuid::parse_str(v).ok());
        let attempts = match id {
            Some(id) => memory.db.report().list_attempts(id).await.unwrap_or_default(),
            None => Vec::new(),
        };
        if let Some(obj) = report.as_object_mut() {
            obj.insert("attempts".to_string(), Value::Array(attempts));
        }
        out.push(report);
    }

    Ok(warp::reply::json(&serde_json::json!({
        "reports": out,
        "count": out.len(),
    }))
    .into_response())
}

/// GET /v1/admin/report/observations — сырые наблюдения для поддержки.
///
/// Юзер пишет «отправил отчёт, ID такой-то» → открываем по подписке и видим
/// по каждой цели: состояние пробы, RTT, ICMP, ноду и версию приложения.
pub async fn admin_observations_handler<N, C, S>(
    query: ObservationsQuery,
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
) -> Result<warp::reply::Response, Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if !admin_enabled {
        return Ok(json_status(404, "not found"));
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(json_status(401, "unauthorized"));
    }

    let subscription = match query.subscription.as_deref() {
        Some(raw) if !raw.trim().is_empty() => match raw.trim().parse::<uuid::Uuid>() {
            Ok(v) => Some(v),
            Err(_) => return Ok(json_status(400, "invalid_subscription")),
        },
        _ => None,
    };
    let address = match query.address.as_deref() {
        Some(raw) if !raw.trim().is_empty() => match raw.trim().parse::<std::net::IpAddr>() {
            Ok(v) => Some(v),
            Err(_) => return Ok(json_status(400, "invalid_address")),
        },
        _ => None,
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 500);

    match memory
        .db
        .report()
        .observations(subscription, address, limit)
        .await
    {
        Ok(rows) => Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 200,
                "response": {
                    "subscription": subscription,
                    "address": address,
                    "count": rows.len(),
                    "observations": rows,
                }
            })),
            warp::http::StatusCode::OK,
        )
        .into_response()),
        Err(err) => {
            tracing::error!("report observations failed: {err}");
            Ok(json_status(500, "observations_failed"))
        }
    }
}

pub async fn admin_block_report_handler<N, C, S>(
    query: BlockReportAggregateQuery,
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
) -> Result<warp::reply::Response, Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if !admin_enabled {
        return Ok(json_status(404, "not found"));
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(json_status(401, "unauthorized"));
    }

    let now = Utc::now();
    let from = query
        .from
        .as_deref()
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.with_timezone(&Utc))
        .unwrap_or_else(|| now - ChronoDuration::hours(1));
    let to = query
        .to
        .as_deref()
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.with_timezone(&Utc))
        .unwrap_or(now);

    let address = match query.address.as_deref() {
        Some(raw) if !raw.trim().is_empty() => match raw.trim().parse::<IpAddr>() {
            Ok(v) => Some(v),
            Err(_) => return Ok(json_status(400, "invalid_address")),
        },
        _ => None,
    };

    let group_by = query.group_by.as_deref().unwrap_or("address");
    if !matches!(group_by, "address" | "port" | "asn" | "country" | "node") {
        return Ok(json_status(400, "invalid_group_by"));
    }

    match memory
        .db
        .report()
        .aggregate(
            from,
            to,
            address,
            query.min_observations.unwrap_or(1),
            group_by,
        )
        .await
    {
        Ok(rows) => Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 200,
                "response": {
                    "from": from,
                    "to": to,
                    "group_by": group_by,
                    "fail_states": FAIL_STATES,
                    "ok_states": OK_STATES,
                    "groups": rows,
                }
            })),
            warp::http::StatusCode::OK,
        )
        .into_response()),
        Err(e) => {
            tracing::error!("block report aggregate failed: {e}");
            Ok(json_status(500, "internal"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_uuid_is_stable_and_unique() {
        let sub = uuid::Uuid::parse_str("3e578494-0163-4f52-a90c-0a477fa2c19e").unwrap();
        assert_eq!(
            batch_uuid("abc-123", Some(sub)),
            batch_uuid("abc-123", Some(sub))
        );
        assert_ne!(
            batch_uuid("abc-123", Some(sub)),
            batch_uuid("abc-124", Some(sub))
        );
        let raw = "d2cd1be7-030e-463a-9fb4-52bf00f9733e";
        assert_eq!(batch_uuid(raw, Some(sub)).to_string(), raw);
    }

    #[test]
    fn empty_address_is_valid_not_an_error() {
        // DNS не ответил — самый ценный отчёт, а не 400.
        assert_eq!(parse_address(&Some(String::new())).unwrap(), None);
        assert_eq!(parse_address(&None).unwrap(), None);
        assert!(parse_address(&Some("93.89.220.49".into()))
            .unwrap()
            .is_some());
        assert!(parse_address(&Some("не-ip".into())).is_err());
    }

    #[test]
    fn prefixes_and_hashes_do_not_leak_ip() {
        assert_eq!(
            ip_prefix("203.0.113.77".parse().unwrap()).unwrap(),
            "203.0.113.0/24"
        );
        assert_eq!(
            ip_prefix("2a02:6b8:c02:1234:5678::1".parse().unwrap()).unwrap(),
            "2a02:6b8:c02::/48"
        );
        let salt = b"frkn-block-telemetry-v1";
        let a = ip_hash("203.0.113.77".parse().unwrap(), salt);
        let b = ip_hash("203.0.113.78".parse().unwrap(), salt);
        assert_eq!(a.len(), 16);
        assert_ne!(a, b);
        assert!(!String::from_utf8_lossy(&a).contains("203.0.113"));
    }

    #[test]
    fn report_state_reads_arrays_and_skips_nothing() {
        let probe = serde_json::json!({
            "dns": {"ips": []},
            "icmp": {"ok": false},
            "tcp": [{"port": 443, "state": "timeout"}],
            "udp": [{"port": 443, "state": "no-answer"}]
        });
        assert_eq!(report_state(&Some(probe.clone())).unwrap(), "no-answer");
        assert_eq!(report_transport(&Some(probe)).unwrap(), "udp");

        // пустой probe — не ошибка
        assert!(report_state(&Some(serde_json::json!({}))).is_none());
        assert!(report_state(&None).is_none());
    }

    #[test]
    fn at_from_future_is_rejected() {
        let future = (Utc::now() + ChronoDuration::hours(2)).to_rfc3339();
        assert_eq!(parse_time(&Some(future)).unwrap_err(), "at_in_future");
        let now = Utc::now().to_rfc3339();
        assert!(parse_time(&Some(now)).unwrap().is_some());
        assert_eq!(
            parse_time(&Some("не-дата".into())).unwrap_err(),
            "invalid_at"
        );
    }

    #[test]
    fn sub_limiter_caps_per_subscription() {
        let limiter = SubRateLimiter::new(2, std::time::Duration::from_secs(60));
        let sub = uuid::Uuid::new_v4();
        assert!(limiter.check(sub));
        assert!(limiter.check(sub));
        assert!(!limiter.check(sub));
        assert!(limiter.check(uuid::Uuid::new_v4()));
    }

    // ------------------------------------------------------------------
    // Новый формат отчёта (клиент 4.8.14.79+)
    // ------------------------------------------------------------------

    /// Реальное тело «туннель выключен → Check available connections».
    const RICH_PROBE: &str = r#"{
      "report_id": "b6c1f0a2-5d3e-4f2a-9a11-7c1f0e2d3b44",
      "support_tag": "023d804b-e677-446e-aba4-d02742fb0e3a",
      "config": { "hash": "19ab2b855b876974", "size": 2009, "rev": "82a5ac09", "api_protocol": "wireguard" },
      "mtu": { "server": 1420, "override": 0, "applied": 1420 },
      "targets": [
        { "address": "217.149.29.118", "port": 51820, "transport": "udp", "state": "unknown",
          "disabled_by_user": false, "in_use": false },
        { "address": "104.171.136.243", "port": 51820, "transport": "udp", "state": "ok", "ms": 16,
          "disabled_by_user": false, "in_use": true }
      ],
      "tcp": [], "udp": [], "dns": { "ips": [] },
      "attempts": [
        { "at": "2026-10-30T21:08:03Z", "address": "217.149.29.118", "port": 51820,
          "protocol": "wireguard", "outcome": "error", "detail": "101", "waited_ms": 428 },
        { "at": "2026-10-30T21:08:41Z", "address": "104.171.136.243", "port": 51820,
          "protocol": "wireguard", "outcome": "ok", "waited_ms": 1030 }
      ]
    }"#;

    /// Реальное тело «туннель включён → Check current connection».
    const RICH_TUNNEL: &str = r#"{
      "report_id": "b6c1f0a2-5d3e-4f2a-9a11-7c1f0e2d3b45",
      "app": { "version": "4.8.14.79", "platform": "ios", "os": "26.6.2" },
      "at": "2026-10-30T21:14:02Z",
      "mode": "tunnel",
      "support_tag": "023d804b-e677-446e-aba4-d02742fb0e3a",
      "config": { "hash": "19ab2b855b876974", "size": 2009, "rev": "82a5ac09", "api_protocol": "wireguard" },
      "mtu": { "server": 1420, "override": 0, "applied": 1420 },
      "network": { "local_ip": "10.66.13.138", "iface": "pdp_ip0", "tunnel_ip": "10.100.16.143" },
      "target": { "host": "217.149.29.118", "port": 51820, "protocol": "wireguard" },
      "tunnel": { "endpoint": "104.171.136.243", "port": 51820, "client_ip": "10.100.16.143", "mtu_applied": 1420 },
      "session": { "connected": true, "connected_at": "2026-10-30T21:10:11Z", "uptime_s": 231,
                   "rtt_ms_avg": 68, "rtt_ms_p95": 68, "samples_ok": 3, "samples_failed": 0,
                   "samples": [66, 68, 70], "dns_check": "ok", "dns_ms": 41,
                   "last_no_data": { "at": "2026-10-30T21:09:02Z", "address": "217.149.29.118",
                                     "port": 51820, "reason": "dns", "rx_bytes": 0, "tx_bytes": 24311 } }
    }"#;

    fn body(v: &str) -> Value {
        serde_json::from_str(v).unwrap()
    }

    /// Старый формат не должен сломаться: диспетчер узнаёт его по объекту
    /// `report` (а не по `report_id` — он есть в обоих форматах).
    #[test]
    fn dispatcher_recognises_legacy_shape() {
        let legacy = body(
            r#"{"report_id":"7d9c1f7e-0000-4000-8000-000000000001",
                "report":{"target":{"address":"1.2.3.4","port":443,"protocol":"awg"}}}"#,
        );
        let is_legacy = |v: &Value| v.get("report").map(|r| r.is_object()).unwrap_or(false);
        assert!(is_legacy(&legacy));

        // новый формат `report`-объекта не имеет, хотя report_id есть
        let rich = body(RICH_TUNNEL);
        assert!(rich.get("report_id").is_some());
        assert!(!is_legacy(&rich));
        assert!(!is_legacy(&body(RICH_PROBE)));
    }

    /// Режим читается и нормализуется; старый клиент поля не шлёт.
    #[test]
    fn mode_is_parsed_and_optional() {
        assert_eq!(report_mode(&body(RICH_TUNNEL)).as_deref(), Some("tunnel"));
        assert_eq!(report_mode(&body(RICH_PROBE)), None);
        let odd = body(r#"{"mode":"  TUNNEL  "}"#);
        assert_eq!(report_mode(&odd).as_deref(), Some("tunnel"));
    }

    /// `report_id` обязателен: без него отчёт не принимается. Сервер не
    /// выдумывает id — иначе повтор после правки одного поля стал бы новым
    /// отчётом, а настоящий повтор мы бы не отличили.
    #[test]
    fn report_id_is_required() {
        let sub = uuid::Uuid::from_u128(0x5150);
        let ip: IpAddr = "5.18.0.1".parse().unwrap();

        let mut without = body(RICH_PROBE);
        without.as_object_mut().unwrap().remove("report_id");
        assert_eq!(
            parse_rich_report(&without, Some(sub), ip).unwrap_err(),
            "missing_report_id"
        );

        let mut broken = body(RICH_PROBE);
        broken["report_id"] = Value::String("не-uuid".into());
        assert_eq!(
            parse_rich_report(&broken, Some(sub), ip).unwrap_err(),
            "invalid_report_id"
        );

        // с валидным id — разбирается
        assert!(parse_rich_report(&body(RICH_PROBE), Some(sub), ip).is_ok());
    }

    /// Тот же `report_id` от клиента = тот же отчёт. Разные id = разные
    /// отчёты, даже если тело совпадает побайтно.
    #[test]
    fn report_id_drives_identity() {
        let sub = uuid::Uuid::from_u128(0x5150);
        let ip: IpAddr = "5.18.0.1".parse().unwrap();
        let first = parse_rich_report(&body(RICH_PROBE), Some(sub), ip).unwrap();
        let repeat = parse_rich_report(&body(RICH_PROBE), Some(sub), ip).unwrap();
        assert_eq!(first.report.id, repeat.report.id);

        let mut other = body(RICH_PROBE);
        other["report_id"] = Value::String("11111111-2222-4333-8444-555555555555".into());
        let different = parse_rich_report(&other, Some(sub), ip).unwrap();
        assert_ne!(first.report.id, different.report.id);
    }

    /// Разбор таргетов: битые записи отбрасываются, годные остаются.
    #[test]
    fn targets_parse_and_reject_broken_entries() {
        let targets: Vec<TargetEntry> = serde_json::from_value(
            body(RICH_PROBE).get("targets").unwrap().clone(),
        )
        .unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].state.as_deref(), Some("unknown"));
        assert_eq!(targets[1].state.as_deref(), Some("ok"));
        assert_eq!(targets[1].ms, Some(16));
        assert!(targets[1].in_use);

        // битый порт и адрес не должны ронять разбор всего отчёта
        let bad = serde_json::json!([
            {"address": "217.149.29.118", "port": 0},
            {"address": "не-адрес", "port": 443},
            {"address": "1.2.3.4", "port": 443, "state": "ok"}
        ]);
        let parsed: Vec<TargetEntry> = serde_json::from_value(bad).unwrap();
        let good: Vec<_> = parsed
            .iter()
            .filter(|t| {
                parse_address(&t.address).ok().flatten().is_some()
                    && t.port.filter(|p| (1..=65535).contains(p)).is_some()
            })
            .collect();
        assert_eq!(good.len(), 1);
        assert_eq!(good[0].port, Some(443));
    }

    /// Попытки разбираются как есть: исход, код, сколько ждали.
    #[test]
    fn attempts_parse_outcome_and_wait() {
        let attempts: Vec<AttemptEntry> =
            serde_json::from_value(body(RICH_PROBE).get("attempts").unwrap().clone()).unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].outcome.as_deref(), Some("error"));
        assert_eq!(attempts[0].detail.as_deref(), Some("101"));
        assert_eq!(attempts[0].waited_ms, Some(428));
        assert_eq!(attempts[1].outcome.as_deref(), Some("ok"));
    }

    /// Словарь клиента попадает в счётчики: `ok` считается успехом, `unknown`
    /// неопределённостью. Иначе отчёты есть, а дашборд пустой.
    #[test]
    fn client_vocabulary_lands_in_counters() {
        use crate::postgres::report::{FAIL_STATES, INCONCLUSIVE_STATES, OK_STATES};
        assert!(OK_STATES.contains(&"ok"));
        assert!(OK_STATES.contains(&"open"));
        assert!(FAIL_STATES.contains(&"error"));
        assert!(INCONCLUSIVE_STATES.contains(&"unknown"));
        assert!(INCONCLUSIVE_STATES.contains(&"no-answer"));
        // и словари не пересекаются — иначе одна строка попадёт в два счётчика
        for state in OK_STATES {
            assert!(!FAIL_STATES.contains(state), "{state} в двух словарях");
            assert!(!INCONCLUSIVE_STATES.contains(state), "{state} в двух словарях");
        }
    }

    /// Тело без таргетов, цели и попыток — не отчёт (при валидном id).
    #[test]
    fn empty_report_is_rejected() {
        let empty = body(
            r#"{"report_id":"7d9c1f7e-0000-4000-8000-000000000002","support_tag":"x"}"#,
        );
        let has_target = empty.get("target").map(|v| !v.is_null()).unwrap_or(false);
        let targets: Vec<TargetEntry> = empty
            .get("targets")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let attempts: Vec<AttemptEntry> = empty
            .get("attempts")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        assert!(targets.is_empty() && !has_target && attempts.is_empty());
    }

    /// Что именно сохранится из реальных тел. Печать:
    ///
    /// ```text
    /// cargo test --bin api report_ingest_sample -- --nocapture
    /// ```
    #[test]
    fn report_ingest_sample() {
        let sub = uuid::Uuid::from_u128(0x5150);
        let ip: IpAddr = "5.18.0.1".parse().unwrap();

        for (name, raw) in [("tunnel off (probes)", RICH_PROBE), ("tunnel on", RICH_TUNNEL)] {
            let parsed = parse_rich_report(&body(raw), Some(sub), ip).unwrap();
            println!("\n=== {name} ===");
            println!(
                "report: id={} mode={:?} app={} platform={} os={:?}",
                parsed.report.id,
                parsed.report.mode,
                parsed.report.app_version,
                parsed.report.platform,
                parsed.report.os
            );
            println!("  config: {}", parsed.report.config);
            println!("  mtu:    {}", parsed.report.mtu);
            if !parsed.report.tunnel.is_null() {
                println!("  tunnel: {}", parsed.report.tunnel);
            }
            println!(
                "  observations: {} (rejected {})",
                parsed.observations.len(),
                parsed.targets_rejected
            );
            for row in &parsed.observations {
                println!(
                    "    [{}] {}:{} proto={} transport={:?} state={:?} rtt={:?}",
                    row.source,
                    row.address.map(|a| a.to_string()).unwrap_or_default(),
                    row.port,
                    row.protocol,
                    row.transport,
                    row.state,
                    row.rtt_ms
                );
            }
            println!("  attempts: {}", parsed.attempts.len());
            for row in &parsed.attempts {
                println!(
                    "    {}:{} {} outcome={:?} detail={:?} waited={:?}ms at={}",
                    row.address.map(|a| a.to_string()).unwrap_or_default(),
                    row.port.unwrap_or(0),
                    row.protocol.clone().unwrap_or_default(),
                    row.outcome,
                    row.detail,
                    row.waited_ms,
                    row.attempted_at.format("%H:%M:%S")
                );
            }
        }
    }

    /// Содержимое, а не только форма: два таргета и две попытки из реального
    /// тела должны дойти до строк без потерь.
    #[test]
    fn rich_probe_body_lands_as_rows() {
        let sub = uuid::Uuid::from_u128(0x5150);
        let ip: IpAddr = "5.18.0.1".parse().unwrap();
        let parsed = parse_rich_report(&body(RICH_PROBE), Some(sub), ip).unwrap();

        assert_eq!(parsed.observations.len(), 2);
        assert_eq!(parsed.targets_rejected, 0);
        assert_eq!(parsed.observations[0].state.as_deref(), Some("unknown"));
        assert_eq!(parsed.observations[1].state.as_deref(), Some("ok"));
        assert_eq!(parsed.observations[1].rtt_ms, Some(16));
        assert_eq!(parsed.observations[0].source, "targets");
        assert_eq!(parsed.observations[0].port, 51820);
        assert_eq!(parsed.observations[0].transport.as_deref(), Some("udp"));

        assert_eq!(parsed.attempts.len(), 2);
        assert_eq!(parsed.attempts[0].outcome.as_deref(), Some("error"));
        assert_eq!(parsed.attempts[0].detail.as_deref(), Some("101"));
        assert_eq!(parsed.attempts[0].waited_ms, Some(428));
        assert_eq!(parsed.attempts[1].outcome.as_deref(), Some("ok"));
        // режим не пришёл — значит старый/безрежимный отчёт
        assert_eq!(parsed.report.mode, None);
        // config/mtu сохранены как есть
        assert_eq!(parsed.report.config["hash"], "19ab2b855b876974");
        assert_eq!(parsed.report.mtu["applied"], 1420);
    }

    /// Тело «туннель включён»: цель одна, режим tunnel, сессия и туннель целы.
    #[test]
    fn rich_tunnel_body_lands_as_rows() {
        let sub = uuid::Uuid::from_u128(0x5150);
        let ip: IpAddr = "5.18.0.1".parse().unwrap();
        let parsed = parse_rich_report(&body(RICH_TUNNEL), Some(sub), ip).unwrap();

        assert_eq!(parsed.report.mode.as_deref(), Some("tunnel"));
        assert_eq!(parsed.report.app_version, "4.8.14.79");
        assert_eq!(parsed.report.platform, "ios");
        assert_eq!(parsed.report.os.as_deref(), Some("26.6.2"));

        // одна строка-наблюдение по цели, без пробы (проб в этом режиме нет)
        assert_eq!(parsed.observations.len(), 1);
        let row = &parsed.observations[0];
        assert_eq!(row.source, "tunnel");
        assert_eq!(row.address.unwrap().to_string(), "217.149.29.118");
        assert_eq!(row.port, 51820);
        assert_eq!(row.protocol, "wireguard");
        assert_eq!(row.state, None, "проб не было — состояния нет");

        // туннель и сессия доехали как есть
        assert_eq!(parsed.report.tunnel["endpoint"], "104.171.136.243");
        assert_eq!(parsed.report.session["rtt_ms_avg"], 68);
        assert_eq!(parsed.report.network["tunnel_ip"], "10.100.16.143");
        assert!(parsed.attempts.is_empty());
    }

    /// Идемпотентность на уровне строк: повторный разбор даёт те же id, значит
    /// повторная отправка не создаст дублей ни в отчётах, ни в попытках.
    #[test]
    fn repeated_ingest_is_idempotent() {
        let sub = uuid::Uuid::from_u128(0x5150);
        let ip: IpAddr = "5.18.0.1".parse().unwrap();
        let first = parse_rich_report(&body(RICH_PROBE), Some(sub), ip).unwrap();
        let second = parse_rich_report(&body(RICH_PROBE), Some(sub), ip).unwrap();
        assert_eq!(first.report.id, second.report.id);
        let ids = |p: &RichParsed| {
            p.attempts.iter().map(|a| a.id).collect::<Vec<_>>()
        };
        assert_eq!(ids(&first), ids(&second));
        let obs = |p: &RichParsed| {
            p.observations.iter().map(|o| o.id).collect::<Vec<_>>()
        };
        assert_eq!(obs(&first), obs(&second));
    }

    /// Диспетчер: старый блок, старый срез и новый формат разводятся по
    /// вложенному объекту, а не по `report_id`.
    #[test]
    fn dispatcher_routes_three_shapes() {
        #[derive(Debug, PartialEq)]
        enum Kind {
            LegacyBlock,
            LegacyScan,
            Rich,
        }
        fn kind(v: &Value) -> Kind {
            if v.get("report").map(|x| x.is_object()).unwrap_or(false) {
                Kind::LegacyBlock
            } else if v.get("scan").map(|x| x.is_object()).unwrap_or(false) {
                Kind::LegacyScan
            } else {
                Kind::Rich
            }
        }

        assert_eq!(
            kind(&body(
                r#"{"report_id":"7d9c1f7e-0000-4000-8000-000000000001",
                    "report":{"target":{"address":"1.2.3.4","port":443,"protocol":"awg"}}}"#
            )),
            Kind::LegacyBlock
        );
        assert_eq!(
            kind(&body(
                r#"{"scan_id":"7d9c1f7e-0000-4000-8000-000000000002",
                    "scan":{"results":[]}}"#
            )),
            Kind::LegacyScan
        );
        // новый формат приходит и в /report/block, и в /report/scan
        assert_eq!(kind(&body(RICH_PROBE)), Kind::Rich);
        assert_eq!(kind(&body(RICH_TUNNEL)), Kind::Rich);
    }

    /// `app` читается с верхнего уровня — так его теперь шлёт клиент в обоих
    /// режимах. Внутри `report`/`scan` остаётся как было (старые сборки).
    #[test]
    fn app_is_read_from_top_level() {
        let sub = uuid::Uuid::from_u128(0x5150);
        let ip: IpAddr = "5.18.0.1".parse().unwrap();
        let parsed = parse_rich_report(&body(RICH_TUNNEL), Some(sub), ip).unwrap();
        assert_eq!(parsed.report.app_version, "4.8.14.79");
        assert_eq!(parsed.report.platform, "ios");
        assert_eq!(parsed.report.os.as_deref(), Some("26.6.2"));

        // старый путь по-прежнему берёт app из report.app
        let legacy: ReportBody = serde_json::from_value(body(
            r#"{"target":{"address":"1.2.3.4","port":443,"protocol":"awg"},
                "app":{"version":"4.8.14.10","platform":"android"}}"#,
        ))
        .unwrap();
        let (version, platform, _os) = app_fields(&legacy.app);
        assert_eq!(version, "4.8.14.10");
        assert_eq!(platform, "android");
    }

    #[test]
    fn trimmed_enforces_limits() {
        assert_eq!(trimmed("  ru  ", 8).unwrap(), "ru");
        assert!(trimmed("   ", 8).is_none());
        assert!(trimmed(&"x".repeat(9), 8).is_none());
    }
}
