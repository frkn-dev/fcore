//! Share tokens — sharing a single connection (`frkn://conn/<token>`).
//!
//! A share token is a scoped credential pointing to a *child* connection
//! created at mint time for the recipient (own UUID/keys, same env+proto as
//! the source, `issued_via = 'share'` in PG). The token authorizes exactly
//! one operation: fetching the child connection's config via POST /v1/config.
//! Everything else (/v1/services, /v1/account_info, /v1/share*) answers 403
//! to a share token; unknown or revoked tokens answer a uniform 404
//! `share_not_found` (anti-enumeration).

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{error, warn};
use warp::Reply;

use fcore::{
    http::helpers as http, Connection, ConnectionApiOperations, ConnectionBaseOperations,
    IpAddrMask, NodeStatus, NodeStorageOperations, Subscription, SubscriptionOperations,
};

use super::super::super::{
    postgres::share::{ShareTokenRow, ISSUED_VIA_SHARE},
    sync::{tasks::SyncOp, MemSync},
};
use super::amnezia::{
    build_gateway_config_response, extract_subscription_id, GatewayConfigParams,
    GatewayConfigRequest,
};
use super::connection::create_connection_inner;

/// Per-subscription cap on active (non-revoked) share tokens.
pub const MAX_SHARES_PER_SUBSCRIPTION: i64 = 20;

/// Crockford base32, lowercase: 16 chars = 80 bits of CSPRNG entropy.
const TOKEN_LEN: usize = 16;
const TOKEN_ALPHABET: &str = "0123456789abcdefghjkmnpqrstvwxyz";

// ============================================================================
// Token format
// ============================================================================

/// Generates a new share token: 80 bits from a CSPRNG, Crockford base32,
/// stored contiguous lowercase.
pub(crate) fn generate_share_token() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 10];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    base32::encode(base32::Alphabet::Crockford, &buf).to_lowercase()
}

/// Display form of a token: `xxxx-xxxx-xxxx-xxxx`.
pub(crate) fn grouped_token(token: &str) -> String {
    token
        .chars()
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// Normalizes a token coming from a client (grouped/uppercase tolerated) to
/// the stored contiguous lowercase form. None = malformed.
pub(crate) fn normalize_share_token(raw: &str) -> Option<String> {
    let normalized: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();

    if normalized.len() == TOKEN_LEN
        && normalized.chars().all(|c| TOKEN_ALPHABET.contains(c))
    {
        Some(normalized)
    } else {
        None
    }
}

/// Trimmed label, 1..=64 chars (user-facing, may be Cyrillic).
pub(crate) fn normalize_share_label(label: &str) -> Result<String, &'static str> {
    let trimmed = label.trim();
    let len = trimmed.chars().count();
    if len == 0 || len > 64 {
        Err("label must be 1..=64 characters")
    } else {
        Ok(trimmed.to_string())
    }
}

// ============================================================================
// Rate limiting (ported from the mrkting service)
// ============================================================================

pub struct RateLimiter {
    limit: usize,
    window: Duration,
    requests: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit,
            window,
            requests: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut map = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let entries = map.entry(ip).or_default();
        entries.retain(|t| now.duration_since(*t) < self.window);
        if entries.len() >= self.limit {
            return false;
        }
        entries.push(now);
        true
    }
}

/// Behind a reverse proxy the peer is always the proxy — prefer the original
/// client IP from X-Forwarded-For (same rule as mrkting).
pub(crate) fn client_ip(remote: Option<SocketAddr>, x_forwarded_for: Option<&str>) -> IpAddr {
    x_forwarded_for
        .and_then(|v| v.split(',').next()?.trim().parse::<IpAddr>().ok())
        .or_else(|| remote.map(|a| a.ip()))
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
}

// ============================================================================
// Small auth/response helpers
// ============================================================================

/// True when auth_data carries a share token instead of a subscription id.
pub(crate) fn auth_data_has_share_token(auth_data: &serde_json::Value) -> bool {
    auth_data
        .get("share_token")
        .and_then(|v| v.as_str())
        .is_some()
}

/// Share-issued child connections are hidden from every owner-facing list
/// (/v1/services, account info device count, the site device list and the
/// whole-subscription link feed).
pub(crate) fn is_share_connection(
    share_conns: &std::collections::HashSet<uuid::Uuid>,
    conn_id: &uuid::Uuid,
) -> bool {
    share_conns.contains(conn_id)
}

/// A share token authorizes /v1/config and nothing else.
pub(crate) fn forbidden(msg: &str) -> warp::reply::Response {
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"status": 403, "message": msg})),
        warp::http::StatusCode::FORBIDDEN,
    )
    .into_response()
}

fn too_many_requests() -> warp::reply::Response {
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"status": 429, "message": "Too many requests"})),
        warp::http::StatusCode::TOO_MANY_REQUESTS,
    )
    .into_response()
}

/// Uniform 404 for unknown/revoked/malformed tokens — no oracle.
fn share_not_found() -> warp::reply::Response {
    http::not_found("share_not_found").into_response()
}

fn fmt_ts(ts: &DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn mint_response(token: &str, label: &str, created_at: &DateTime<Utc>) -> warp::reply::Response {
    warp::reply::json(&serde_json::json!({
        "share_token": token,
        "share_url": format!("frkn://conn/{}", grouped_token(token)),
        "label": label,
        "created_at": fmt_ts(created_at),
    }))
    .into_response()
}

// ============================================================================
// Mint decision (idempotency + limit), extracted for tests
// ============================================================================

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MintDecision {
    /// A live token for the same (connection, node, label) exists: return it.
    ReturnExisting,
    LimitReached,
    Mint,
}

pub(crate) fn mint_decision(has_active_triple: bool, active_count: i64) -> MintDecision {
    if has_active_triple {
        MintDecision::ReturnExisting
    } else if active_count >= MAX_SHARES_PER_SUBSCRIPTION {
        MintDecision::LimitReached
    } else {
        MintDecision::Mint
    }
}

// ============================================================================
// Requests
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct ShareMintRequest {
    pub auth_data: serde_json::Value,
    pub connection_uuid: uuid::Uuid,
    pub node_id: uuid::Uuid,
    pub label: String,
}

#[derive(Debug, Deserialize)]
pub struct ShareListRequest {
    pub auth_data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ShareRevokeRequest {
    pub auth_data: serde_json::Value,
    pub share_token: String,
}

/// POST /share (mgmt, service token) — the site's per-device share link.
#[derive(Debug, Deserialize)]
pub struct MgmtShareMintRequest {
    pub connection_id: uuid::Uuid,
    pub label: String,
}

/// POST /share/revoke (mgmt, service token).
#[derive(Debug, Deserialize)]
pub struct MgmtShareRevokeRequest {
    pub share_token: String,
    pub subscription_id: uuid::Uuid,
}

// ============================================================================
// Handlers
// ============================================================================

/// Best-effort removal of a just-minted child connection after a mint step
/// failed halfway; keeps no orphan connection behind.
async fn delete_child_connection<N, C, S>(
    memory: &MemSync<N, C, S>,
    child_id: &uuid::Uuid,
    child: Connection,
) where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
    Connection: From<C>,
{
    let conn: C = child.into();
    if let Err(e) = SyncOp::delete_connection(memory, child_id, &conn).await {
        error!(
            "share mint rollback: failed to delete child connection {}: {}",
            child_id, e
        );
    }
}

/// POST /v1/share — mint a share token (owner).
pub async fn gateway_share_mint_handler<N, C, S>(
    req: ShareMintRequest,
    memory: MemSync<N, C, S>,
    remote: Option<SocketAddr>,
    x_forwarded_for: Option<String>,
    rate_limiter: Arc<RateLimiter>,
    wg_network: IpAddrMask,
    awg_network: IpAddrMask,
    awg_mobile_network: Option<IpAddrMask>,
) -> Result<warp::reply::Response, warp::Rejection>
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
    if !rate_limiter.check(ip) {
        return Ok(too_many_requests());
    }

    // No transitive minting: a share token cannot mint further shares.
    if auth_data_has_share_token(&req.auth_data) {
        return Ok(forbidden("share_token is only valid for /v1/config"));
    }

    let sub_id = match extract_subscription_id(&req.auth_data) {
        Some(id) => id,
        None => {
            return Ok(http::bad_request("Missing subscription id in auth_data").into_response())
        }
    };

    let label = match normalize_share_label(&req.label) {
        Ok(l) => l,
        Err(msg) => return Ok(http::bad_request(msg).into_response()),
    };

    // The source connection decides env+proto; both it and the node must
    // belong to this subscription (the node via the connection's env).
    let (env, proto) = {
        let mem = memory.memory.read().await;

        let source = mem
            .connections
            .get(&req.connection_uuid)
            .filter(|c| !c.get_deleted() && c.get_subscription_id() == Some(sub_id));
        let Some(source) = source else {
            return Ok(http::not_found("connection_not_found").into_response());
        };

        let env = source.get_env();
        let proto = source.get_proto().proto();

        let node_known = mem
            .nodes
            .get_by_env(&env)
            .map(|nodes| nodes.iter().any(|n| n.uuid == req.node_id))
            .unwrap_or(false);
        if !node_known {
            return Ok(http::not_found("connection_not_found").into_response());
        }

        (env, proto)
    };

    match mint_share_inner(
        &memory,
        sub_id,
        req.connection_uuid,
        req.node_id,
        env,
        proto,
        label,
        &wg_network,
        &awg_network,
        &awg_mobile_network,
    )
    .await
    {
        Ok(row) => Ok(mint_response(&row.token, &row.label, &row.created_at)),
        Err(resp) => Ok(resp),
    }
}

/// Shared mint core for /v1/share (app, AGW envelope) and POST /share
/// (mgmt): idempotent per (source, node, label) triple, limited to
/// MAX_SHARES_PER_SUBSCRIPTION active tokens per subscription. Callers
/// validate that the source connection and node are legit; `env`/`proto`
/// come from the source connection.
pub(crate) async fn mint_share_inner<N, C, S>(
    memory: &MemSync<N, C, S>,
    sub_id: uuid::Uuid,
    source_connection_id: uuid::Uuid,
    node_id: uuid::Uuid,
    env: fcore::Env,
    proto: fcore::Tag,
    label: String,
    wg_network: &IpAddrMask,
    awg_network: &IpAddrMask,
    awg_mobile_network: &Option<IpAddrMask>,
) -> Result<ShareTokenRow, warp::reply::Response>
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
    let db = memory.db.share();

    let existing = match db.find_active(&source_connection_id, &node_id, &label).await {
        Ok(v) => v,
        Err(e) => {
            error!("share mint: lookup failed for sub {}: {}", sub_id, e);
            return Err(http::internal_error("share lookup failed").into_response());
        }
    };

    let active_count = match db.count_active(&sub_id).await {
        Ok(v) => v,
        Err(e) => {
            error!("share mint: count failed for sub {}: {}", sub_id, e);
            return Err(http::internal_error("share lookup failed").into_response());
        }
    };

    match mint_decision(existing.is_some(), active_count) {
        MintDecision::ReturnExisting => {
            // Idempotency: never create a duplicate child connection.
            if let Some(row) = existing {
                return Ok(row);
            }
        }
        MintDecision::LimitReached => {
            return Err(http::conflict("share_limit_reached").into_response())
        }
        MintDecision::Mint => {}
    }

    // The child connection inherits the subscription lifetime (days=None)
    // and carries the share label like a named device.
    let (child_id, child) = match create_connection_inner(
        &env,
        proto,
        Some(sub_id),
        None,
        Some(label.clone()),
        memory,
        wg_network,
        awg_network,
        awg_mobile_network,
    )
    .await
    {
        Ok(v) => v,
        Err(msg) => {
            if msg.contains("not found") || msg.contains("not active") || msg.contains("IP out of range") {
                return Err(http::bad_request(&msg).into_response());
            }
            return Err(http::internal_error(&msg).into_response());
        }
    };

    if let Err(e) = memory
        .db
        .conn()
        .set_issued_via(&child_id, ISSUED_VIA_SHARE)
        .await
    {
        error!("share mint: issued_via flag failed for sub {}: {}", sub_id, e);
        delete_child_connection(memory, &child_id, child).await;
        return Err(http::internal_error("share mint failed").into_response());
    }

    {
        let mut mem = memory.memory.write().await;
        mem.share_conns.insert(child_id);
    }

    // Insert the token row; on a lost mint race answer with the winner's
    // token, on a primary-key collision retry with fresh entropy.
    for _ in 0..3 {
        let row = ShareTokenRow {
            token: generate_share_token(),
            subscription_id: sub_id,
            connection_id: child_id,
            node_id,
            source_connection_id,
            label: label.clone(),
            created_at: Utc::now(),
            last_used_at: None,
            revoked_at: None,
        };

        match db.insert(&row).await {
            Ok(true) => return Ok(row),
            Ok(false) => {
                match db.find_active(&source_connection_id, &node_id, &label).await {
                    Ok(Some(winner)) => {
                        delete_child_connection(memory, &child_id, child).await;
                        return Ok(winner);
                    }
                    Ok(None) => continue, // token PK collision — try again
                    Err(e) => {
                        error!("share mint: re-read failed for sub {}: {}", sub_id, e);
                        delete_child_connection(memory, &child_id, child).await;
                        return Err(http::internal_error("share mint failed").into_response());
                    }
                }
            }
            Err(e) => {
                error!("share mint: insert failed for sub {}: {}", sub_id, e);
                delete_child_connection(memory, &child_id, child).await;
                return Err(http::internal_error("share mint failed").into_response());
            }
        }
    }

    error!("share mint: token allocation retries exhausted for sub {}", sub_id);
    delete_child_connection(memory, &child_id, child).await;
    Err(http::internal_error("failed to allocate share token").into_response())
}

/// POST /v1/shares — list active share tokens (owner).
pub async fn gateway_shares_list_handler<N, C, S>(
    req: ShareListRequest,
    memory: MemSync<N, C, S>,
    remote: Option<SocketAddr>,
    x_forwarded_for: Option<String>,
    rate_limiter: Arc<RateLimiter>,
) -> Result<warp::reply::Response, warp::Rejection>
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
    if !rate_limiter.check(ip) {
        return Ok(too_many_requests());
    }

    if auth_data_has_share_token(&req.auth_data) {
        return Ok(forbidden("share_token is only valid for /v1/config"));
    }

    let sub_id = match extract_subscription_id(&req.auth_data) {
        Some(id) => id,
        None => {
            return Ok(http::bad_request("Missing subscription id in auth_data").into_response())
        }
    };

    let rows = match memory.db.share().list_active(&sub_id).await {
        Ok(rows) => rows,
        Err(e) => {
            error!("share list failed for sub {}: {}", sub_id, e);
            return Ok(http::internal_error("share list failed").into_response());
        }
    };

    let shares: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "share_token": r.token,
                "label": r.label,
                "node_id": r.node_id,
                "created_at": fmt_ts(&r.created_at),
                "last_used_at": r.last_used_at.as_ref().map(fmt_ts),
            })
        })
        .collect();

    Ok(warp::reply::json(&serde_json::json!({ "shares": shares })).into_response())
}

/// POST /v1/share/revoke — revoke a token and delete its child connection.
pub async fn gateway_share_revoke_handler<N, C, S>(
    req: ShareRevokeRequest,
    memory: MemSync<N, C, S>,
    remote: Option<SocketAddr>,
    x_forwarded_for: Option<String>,
    rate_limiter: Arc<RateLimiter>,
) -> Result<warp::reply::Response, warp::Rejection>
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
    if !rate_limiter.check(ip) {
        return Ok(too_many_requests());
    }

    if auth_data_has_share_token(&req.auth_data) {
        return Ok(forbidden("share_token is only valid for /v1/config"));
    }

    let sub_id = match extract_subscription_id(&req.auth_data) {
        Some(id) => id,
        None => {
            return Ok(http::bad_request("Missing subscription id in auth_data").into_response())
        }
    };

    let Some(token) = normalize_share_token(&req.share_token) else {
        return Ok(share_not_found());
    };

    if let Err(resp) = revoke_share_inner(&memory, &token, sub_id).await {
        return Ok(resp);
    }

    Ok(warp::reply::json(&serde_json::json!({
        "status": 200,
        "message": "share_revoked"
    }))
    .into_response())
}

/// Shared revoke core for /v1/share/revoke (app) and POST /share/revoke
/// (mgmt): marks the token revoked and deletes its child connection.
/// Unknown, already revoked or cross-subscription tokens get a uniform 404
/// share_not_found (the caller's subscription id is the ownership proof).
pub(crate) async fn revoke_share_inner<N, C, S>(
    memory: &MemSync<N, C, S>,
    token: &str,
    sub_id: uuid::Uuid,
) -> Result<(), warp::reply::Response>
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
    let row = match memory.db.share().get(token).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(share_not_found()),
        Err(e) => {
            error!("share revoke: lookup failed for sub {}: {}", sub_id, e);
            return Err(http::internal_error("share revoke failed").into_response());
        }
    };

    if row.revoked_at.is_some() || row.subscription_id != sub_id {
        return Err(share_not_found());
    }

    if let Err(e) = memory.db.share().revoke(token).await {
        error!("share revoke: update failed for sub {}: {}", sub_id, e);
        return Err(http::internal_error("share revoke failed").into_response());
    }

    // Drop the child connection from the node (ZMQ delete) and from memory.
    // Best-effort: an already-gone child does not fail the revoke.
    let child = {
        let mem = memory.memory.read().await;
        mem.connections.get(&row.connection_id).cloned()
    };
    if let Some(conn) = child {
        if !conn.get_deleted() {
            delete_child_connection(memory, &row.connection_id, conn.into()).await;
        }
    }

    Ok(())
}

/// The share_token branch of POST /v1/config: the config is built strictly
/// for the token's child connection on its node — connection_id/node_id in
/// the payload are ignored.
pub async fn gateway_share_config_handler<N, C, S>(
    req: GatewayConfigRequest,
    raw_token: &str,
    memory: MemSync<N, C, S>,
    remote: Option<SocketAddr>,
    x_forwarded_for: Option<String>,
    rate_limiter: Arc<RateLimiter>,
) -> warp::reply::Response
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq,
{
    let ip = client_ip(remote, x_forwarded_for.as_deref());
    if !rate_limiter.check(ip) {
        return too_many_requests();
    }

    let token = match normalize_share_token(raw_token) {
        Some(t) => t,
        None => {
            // A miss spike from one IP = enumeration attempt. Never log the
            // token itself.
            warn!("share config: malformed token from {}", ip);
            return share_not_found();
        }
    };

    let row = match memory.db.share().get(&token).await {
        Ok(Some(row)) if row.revoked_at.is_none() => row,
        Ok(_) => {
            warn!("share config: unknown/revoked token from {}", ip);
            return share_not_found();
        }
        Err(e) => {
            error!("share config: token lookup failed: {}", e);
            return http::internal_error("share lookup failed").into_response();
        }
    };

    let params = GatewayConfigParams {
        service_protocol: &req.service_protocol,
        service_type: &req.service_type,
        user_country_code: req.user_country_code.as_deref(),
        server_country_code: req.server_country_code.as_deref(),
        connection_id: Some(row.connection_id),
        node_id: Some(row.node_id),
    };

    match build_gateway_config_response(&memory, &row.subscription_id, &params).await {
        Ok(mut resp) => {
            // The recipient has no /v1/services access — include the display
            // fields the app needs to render the imported server.
            let country = {
                let mem = memory.memory.read().await;
                mem.nodes
                    .get_by_id(&row.node_id)
                    .map(|n| n.country.to_uppercase())
            };
            resp.share_label = Some(row.label.clone());
            resp.country_code = country.clone();
            resp.country_name = country;
            resp.service_protocol = resp
                .api_config
                .get("service_protocol")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // Cheap async bookkeeping; must not block or fail the request.
            let db = memory.db.clone();
            tokio::spawn(async move {
                if let Err(e) = db.share().touch_last_used(&token).await {
                    warn!("share last_used_at update failed: {}", e);
                }
            });

            warp::reply::json(&resp).into_response()
        }
        Err(resp) => resp,
    }
}

// ============================================================================
// Mgmt handlers (service-token auth; the site's proxy in mrkting uses these)
// ============================================================================

/// First online node exposing a matching inbound — the same selection rule
/// as the subscription feed builder. Candidates are (uuid, is_online,
/// has_matching_inbound) triples so the rule stays unit-testable.
pub(crate) fn pick_share_node(candidates: &[(uuid::Uuid, bool, bool)]) -> Option<uuid::Uuid> {
    candidates
        .iter()
        .find(|(_, is_online, has_inbound)| *is_online && *has_inbound)
        .map(|(id, _, _)| *id)
}

/// POST /share — mint a share token via mgmt (service token) auth. Unlike
/// /v1/share the node is auto-picked: the first online node of the source
/// connection's env with a matching inbound.
pub async fn mgmt_share_mint_handler<N, C, S>(
    req: MgmtShareMintRequest,
    memory: MemSync<N, C, S>,
    wg_network: IpAddrMask,
    awg_network: IpAddrMask,
    awg_mobile_network: Option<IpAddrMask>,
) -> Result<warp::reply::Response, warp::Rejection>
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
    let label = match normalize_share_label(&req.label) {
        Ok(l) => l,
        Err(msg) => return Ok(http::bad_request(msg).into_response()),
    };

    let (sub_id, env, proto, node_id) = {
        let mem = memory.memory.read().await;

        let conn = mem
            .connections
            .get(&req.connection_id)
            .filter(|c| !c.get_deleted());
        let Some(conn) = conn else {
            return Ok(http::not_found("connection_not_found").into_response());
        };
        // A connection without a subscription cannot be shared: the token
        // row and the per-subscription limit both need it.
        let Some(sub_id) = conn.get_subscription_id() else {
            return Ok(http::not_found("connection_not_found").into_response());
        };

        let env = conn.get_env();
        let proto = conn.get_proto().proto();

        let node_id = mem.nodes.get_by_env(&env).and_then(|nodes| {
            pick_share_node(
                &nodes
                    .iter()
                    .map(|n| {
                        (
                            n.uuid,
                            n.status == NodeStatus::Online,
                            n.inbounds.contains_key(&proto),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        });
        let Some(node_id) = node_id else {
            return Ok(http::not_found("node_not_found").into_response());
        };

        (sub_id, env, proto, node_id)
    };

    match mint_share_inner(
        &memory,
        sub_id,
        req.connection_id,
        node_id,
        env,
        proto,
        label,
        &wg_network,
        &awg_network,
        &awg_mobile_network,
    )
    .await
    {
        Ok(row) => Ok(mint_response(&row.token, &row.label, &row.created_at)),
        Err(resp) => Ok(resp),
    }
}

/// POST /share/revoke — revoke via mgmt auth. Revokes only when the token's
/// subscription matches the request's; unknown/revoked/mismatched tokens all
/// get the uniform 404 share_not_found.
pub async fn mgmt_share_revoke_handler<N, C, S>(
    req: MgmtShareRevokeRequest,
    memory: MemSync<N, C, S>,
) -> Result<warp::reply::Response, warp::Rejection>
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
    let Some(token) = normalize_share_token(&req.share_token) else {
        return Ok(share_not_found());
    };

    if let Err(resp) = revoke_share_inner(&memory, &token, req.subscription_id).await {
        return Ok(resp);
    }

    Ok(warp::reply::json(&serde_json::json!({
        "status": 200,
        "message": "share_revoked"
    }))
    .into_response())
}

#[cfg(test)]
mod tests {    use super::*;

    #[test]
    fn test_generate_share_token_charset_and_length() {
        for _ in 0..100 {
            let token = generate_share_token();
            assert_eq!(token.len(), TOKEN_LEN);
            assert!(token.chars().all(|c| TOKEN_ALPHABET.contains(c)));
            assert_eq!(token, token.to_lowercase());
        }
    }

    #[test]
    fn test_generate_share_token_uniqueness() {
        let a = generate_share_token();
        let b = generate_share_token();
        assert_ne!(a, b);
    }

    #[test]
    fn test_grouped_token() {
        assert_eq!(grouped_token("k7f29mxq4tvzabcd"), "k7f2-9mxq-4tvz-abcd");
    }

    #[test]
    fn test_normalize_share_token_accepts_display_forms() {
        let bare = "k7f29mxq4tvzabcd";
        assert_eq!(normalize_share_token(bare).as_deref(), Some(bare));
        assert_eq!(
            normalize_share_token("k7f2-9mxq-4tvz-abcd").as_deref(),
            Some(bare)
        );
        assert_eq!(
            normalize_share_token("K7F29MXQ4TVZABCD").as_deref(),
            Some(bare)
        );
    }

    #[test]
    fn test_normalize_share_token_rejects_garbage() {
        assert_eq!(normalize_share_token(""), None);
        assert_eq!(normalize_share_token("k7f2"), None);
        // 'o', 'i', 'l', 'u' are outside the Crockford alphabet.
        assert_eq!(normalize_share_token("o7f29mxq4tvzabcd"), None);
        assert_eq!(normalize_share_token("k7f29mxq4tvzabcde"), None);
    }

    #[test]
    fn test_normalize_share_label() {
        assert_eq!(
            normalize_share_label("  Android Mama Wireguard  ").as_deref(),
            Ok("Android Mama Wireguard")
        );
        assert!(normalize_share_label("   ").is_err());
        assert!(normalize_share_label("").is_err());
        assert!(normalize_share_label(&"я".repeat(64)).is_ok());
        assert!(normalize_share_label(&"я".repeat(65)).is_err());
    }

    #[test]
    fn test_mint_decision() {
        assert_eq!(mint_decision(true, 0), MintDecision::ReturnExisting);
        // Idempotency wins over the limit: an existing token is returned
        // even when the subscription is at the cap.
        assert_eq!(
            mint_decision(true, MAX_SHARES_PER_SUBSCRIPTION),
            MintDecision::ReturnExisting
        );
        assert_eq!(mint_decision(false, 0), MintDecision::Mint);
        assert_eq!(
            mint_decision(false, MAX_SHARES_PER_SUBSCRIPTION - 1),
            MintDecision::Mint
        );
        assert_eq!(
            mint_decision(false, MAX_SHARES_PER_SUBSCRIPTION),
            MintDecision::LimitReached
        );
    }

    #[test]
    fn test_is_share_connection() {
        let child = uuid::Uuid::new_v4();
        let normal = uuid::Uuid::new_v4();
        let share_conns: std::collections::HashSet<uuid::Uuid> = [child].into_iter().collect();

        assert!(is_share_connection(&share_conns, &child));
        assert!(!is_share_connection(&share_conns, &normal));
    }

    #[test]
    fn test_pick_share_node() {
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        let c = uuid::Uuid::new_v4();

        // First online node with a matching inbound wins.
        let candidates = vec![
            (a, false, true),  // offline
            (b, true, false),  // no matching inbound
            (c, true, true),
        ];
        assert_eq!(pick_share_node(&candidates), Some(c));

        // Order matters: the first candidate wins.
        let candidates = vec![(c, true, true), (b, true, true)];
        assert_eq!(pick_share_node(&candidates), Some(c));

        assert_eq!(pick_share_node(&[(a, false, true)]), None);
        assert_eq!(pick_share_node(&[]), None);
    }
}
