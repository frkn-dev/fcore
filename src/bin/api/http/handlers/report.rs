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

use super::amnezia::extract_subscription_id;
use super::share::{auth_data_has_share_token, client_ip, RateLimiter};

/// Лимит тела одиночного отчёта.
pub const MAX_REPORT_BODY_BYTES: u64 = 16 * 1024;
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

fn observation_uuid(batch: uuid::Uuid, index: usize, address: Option<IpAddr>, host: Option<&str>) -> uuid::Uuid {
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
        Some(s) => s.parse::<IpAddr>().map(Some).map_err(|_| "invalid_address".into()),
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

    let (asn, client_country) = fcore::geoip::lookup(ip);
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
        app: req.report.app.clone().unwrap_or(Value::Null),
        device: req.report.device.clone().unwrap_or(Value::Null),
        network: req.report.network.clone().unwrap_or(Value::Null),
        probe: req.report.probe.clone().unwrap_or(Value::Null),
        session: req.report.session.clone().unwrap_or(Value::Null),
        country_code: opt_trimmed(&target.country_code, 2).or_else(|| opt_trimmed(&target.country_code, 3)),
        asn: asn.map(|v| v as i32),
        client_country,
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

    match memory.db.report().insert_batch(batch_id, std::slice::from_ref(&row)).await {
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

    let (asn, client_country) = fcore::geoip::lookup(ip);
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

pub async fn admin_block_report_handler<N, C, S>(
    query: BlockReportAggregateQuery,
    memory: MemSync<N, C, S>,
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
        assert!(parse_address(&Some("93.89.220.49".into())).unwrap().is_some());
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
        assert_eq!(parse_time(&Some("не-дата".into())).unwrap_err(), "invalid_at");
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

    #[test]
    fn trimmed_enforces_limits() {
        assert_eq!(trimmed("  ru  ", 8).unwrap(), "ru");
        assert!(trimmed("   ", 8).is_none());
        assert!(trimmed(&"x".repeat(9), 8).is_none());
    }
}
