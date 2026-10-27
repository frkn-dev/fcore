//! Отчёты клиента о блокировке адреса (`POST /v1/report/block`).
//!
//! Приём намеренно plaintext (без `with_agw_decryption`): в payload нет
//! секретов, а поддержке так проще отлаживать. Тело ≤ 16 КБ, лимиты по IP и
//! по подписке, идемпотентность по клиентскому `report_id`, отчёты «из
//! будущего» отбрасываются.
//!
//! IP пользователя не пишем: только ASN, страна, префикс /24 (/48) и
//! blake3-хеш для счёта уникальности.

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

use crate::postgres::report::BlockReportRow;
use crate::sync::MemSync;

use super::amnezia::extract_subscription_id;
use super::share::{auth_data_has_share_token, client_ip, RateLimiter};

/// Максимальный размер тела отчёта.
pub const MAX_REPORT_BODY_BYTES: u64 = 16 * 1024;
/// Допустимое опережение по часам клиента.
const MAX_CLOCK_SKEW_SECS: i64 = 300;
/// Дедуп по окну: сколько секунд считаем отчёты по паре (адрес, порт) «тем же».
pub const DEDUP_WINDOW_SECS: i64 = 600;

#[derive(Debug, Deserialize)]
pub struct BlockReportRequest {
    pub auth_data: Value,
    pub report_id: String,
    pub target: ReportTarget,
    #[serde(default)]
    pub probe: Option<Value>,
    #[serde(default)]
    pub app: Option<Value>,
    #[serde(default)]
    pub support_tag: Option<String>,
    #[serde(default)]
    pub error: Option<Value>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReportTarget {
    pub address: String,
    pub port: i64,
    pub protocol: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
}

/// Лимит по подписке: 30 отчётов в час. Ключ — subscription uuid.
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

fn json_status(code: u16, message: &str) -> warp::reply::Response {
    let status = warp::http::StatusCode::from_u16(code).unwrap_or(warp::http::StatusCode::BAD_REQUEST);
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({ "status": code, "message": message })),
        status,
    )
    .into_response()
}

/// `report_id` приходит строкой; в PG ключ — uuid, поэтому не-uuid значения
/// стабильно сворачиваем в uuid из хеша (идемпотентность сохраняется).
fn report_uuid(report_id: &str, sub_id: Option<uuid::Uuid>) -> uuid::Uuid {
    if let Ok(id) = uuid::Uuid::parse_str(report_id) {
        return id;
    }
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    if let Some(sub) = sub_id {
        hasher.update(sub.as_bytes());
    }
    hasher.update(report_id.as_bytes());
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

/// POST /v1/report/block
pub async fn gateway_block_report_handler<N, C, S>(
    req: BlockReportRequest,
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
    let has_share = auth_data_has_share_token(&req.auth_data);
    if sub_id.is_none() && !has_share {
        return Ok(json_status(401, "unauthorized"));
    }
    if let Some(sub) = sub_id {
        if !sub_limiter.check(sub) {
            return Ok(json_status(429, "too_many_requests_subscription"));
        }
    }

    // target
    let address: IpAddr = match req.target.address.trim().parse() {
        Ok(v) => v,
        Err(_) => return Ok(json_status(400, "invalid_address")),
    };
    if req.target.port < 1 || req.target.port > 65535 {
        return Ok(json_status(400, "invalid_port"));
    }
    let protocol = match trimmed(&req.target.protocol, 32) {
        Some(v) => v,
        None => return Ok(json_status(400, "invalid_protocol")),
    };

    // at: не из будущего
    let now = Utc::now();
    let report_at = match req.at.as_deref() {
        Some(raw) => match DateTime::parse_from_rfc3339(raw) {
            Ok(dt) => {
                let dt = dt.with_timezone(&Utc);
                if dt > now + ChronoDuration::seconds(MAX_CLOCK_SKEW_SECS) {
                    return Ok(json_status(400, "at_in_future"));
                }
                dt
            }
            Err(_) => return Ok(json_status(400, "invalid_at")),
        },
        None => now,
    };

    let app = req.app.unwrap_or(Value::Null);
    let probe = req.probe.unwrap_or(Value::Null);
    let error = req.error.unwrap_or(Value::Null);

    let app_version = app
        .get("version")
        .and_then(|v| v.as_str())
        .and_then(|v| trimmed(v, 32))
        .unwrap_or_else(|| "unknown".to_string());
    let platform = app
        .get("platform")
        .and_then(|v| v.as_str())
        .and_then(|v| trimmed(v, 32))
        .unwrap_or_else(|| "unknown".to_string());
    let os = app
        .get("os")
        .and_then(|v| v.as_str())
        .and_then(|v| trimmed(v, 64));

    let (node_id, node_host) = memory
        .db
        .report()
        .resolve_node(address)
        .await
        .unwrap_or((None, None));

    let salt = b"frkn-block-telemetry-v1";
    let row = BlockReportRow {
        id: report_uuid(&req.report_id, sub_id),
        subscription_id: sub_id,
        share_token_hash: None,
        support_tag: req.support_tag.as_deref().and_then(|v| trimmed(v, 64)),
        app_version,
        platform,
        os,
        node_address: address,
        node_id,
        node_host,
        port: req.target.port as i32,
        protocol,
        transport: req.target.transport.as_deref().and_then(|v| trimmed(v, 16)),
        error_code: error.get("code").and_then(|v| v.as_i64()).map(|v| v as i32),
        error_stage: error
            .get("stage")
            .and_then(|v| v.as_str())
            .and_then(|v| trimmed(v, 32)),
        icmp_ok: probe
            .get("icmp")
            .and_then(|v| v.get("ok"))
            .and_then(|v| v.as_bool()),
        icmp_rtt_ms: probe
            .get("icmp")
            .and_then(|v| v.get("rtt_ms"))
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        dns_ips: probe.get("dns").cloned().unwrap_or(Value::Null),
        tcp_probe: probe.get("tcp").cloned().unwrap_or(Value::Null),
        udp_probe: probe.get("udp").cloned().unwrap_or(Value::Null),
        device: app.get("device").cloned().unwrap_or(Value::Null),
        user_note: req.note.as_deref().and_then(|v| trimmed(v, 500)),
        asn: None,
        country: None,
        ip_prefix: ip_prefix(ip),
        ip_hash: Some(ip_hash(ip, salt)),
        report_at,
    };

    // дедуп по окну: тот же адрес+порт от той же подписки недавно уже был
    if let Ok(count) = memory
        .db
        .report()
        .count_recent(address, row.port, sub_id, DEDUP_WINDOW_SECS)
        .await
    {
        if count > 0 {
            return Ok(json_status(202, "dedup:window"));
        }
    }

    match memory.db.report().insert_once(&row).await {
        Ok(true) => Ok(json_status(202, "accepted")),
        Ok(false) => Ok(json_status(202, "dedup:report_id")),
        Err(e) => {
            tracing::error!("block report insert failed: {e}");
            Ok(json_status(500, "internal"))
        }
    }
}

/// GET /v1/admin/report/block?from=&to=&group_by=&address=&min_reports=
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
    pub min_reports: Option<i64>,
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
        Some(raw) => match raw.trim().parse::<IpAddr>() {
            Ok(v) => Some(v),
            Err(_) => return Ok(json_status(400, "invalid_address")),
        },
        None => None,
    };

    let group_by = query.group_by.as_deref().unwrap_or("address");
    if !matches!(group_by, "address" | "port" | "asn" | "country" | "node") {
        return Ok(json_status(400, "invalid_group_by"));
    }

    match memory
        .db
        .report()
        .aggregate(from, to, address, query.min_reports.unwrap_or(1), group_by)
        .await
    {
        Ok(rows) => Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 200,
                "response": {
                    "from": from,
                    "to": to,
                    "group_by": group_by,
                    "polygon_age_sec": serde_json::Value::Null,
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
    fn report_uuid_is_stable_and_unique() {
        let sub = uuid::Uuid::parse_str("3e578494-0163-4f52-a90c-0a477fa2c19e").unwrap();
        let a = report_uuid("abc-123", Some(sub));
        let b = report_uuid("abc-123", Some(sub));
        let c = report_uuid("abc-124", Some(sub));
        assert_eq!(a, b, "одинаковый report_id должен давать тот же ключ");
        assert_ne!(a, c);
        // валидный uuid проходит как есть
        let raw = "d2cd1be7-030e-463a-9fb4-52bf00f9733e";
        assert_eq!(report_uuid(raw, Some(sub)).to_string(), raw);
    }

    #[test]
    fn prefixes_are_truncated_not_full_ip() {
        assert_eq!(
            ip_prefix("203.0.113.77".parse().unwrap()).unwrap(),
            "203.0.113.0/24"
        );
        assert_eq!(
            ip_prefix("2a02:6b8:c02:1234:5678::1".parse().unwrap()).unwrap(),
            "2a02:6b8:c02::/48"
        );
    }

    #[test]
    fn ip_hash_differs_for_different_users() {
        let salt = b"frkn-block-telemetry-v1";
        let a = ip_hash("203.0.113.77".parse().unwrap(), salt);
        let b = ip_hash("203.0.113.78".parse().unwrap(), salt);
        assert_eq!(a.len(), 16);
        assert_ne!(a, b);
        // в хеше не должно быть читаемого ip
        assert!(!String::from_utf8_lossy(&a).contains("203.0.113"));
    }

    #[test]
    fn trimmed_enforces_limits() {
        assert_eq!(trimmed("  ru  ", 8).unwrap(), "ru");
        assert!(trimmed("   ", 8).is_none());
        assert!(trimmed(&"x".repeat(9), 8).is_none());
    }

    #[test]
    fn sub_limiter_caps_per_subscription() {
        let limiter = SubRateLimiter::new(2, std::time::Duration::from_secs(60));
        let sub = uuid::Uuid::new_v4();
        assert!(limiter.check(sub));
        assert!(limiter.check(sub));
        assert!(!limiter.check(sub));
        // другая подписка не затронута
        assert!(limiter.check(uuid::Uuid::new_v4()));
    }
}
