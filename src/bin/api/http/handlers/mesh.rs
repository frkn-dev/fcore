//! Mesh relay (/v1/mesh/*) — last-mile E2E messenger. The server never
//! sees plaintext and keeps no history: only an opaque-ciphertext inbox in
//! RAM and a persistent UIN → pubkey registry (see fcore::mesh).

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use warp::Reply;

use fcore::http::helpers as http;
use fcore::mesh::{MeshError, MeshRegistry};

use super::share::{client_ip, RateLimiter};

const MAX_BODY_BYTES: u64 = 1024 * 1024;

pub fn body_limit() -> impl warp::Filter<Extract = (), Error = warp::Rejection> + Clone {
    warp::body::content_length_limit(MAX_BODY_BYTES)
}

#[derive(Debug, Deserialize)]
pub struct MeshRegisterRequest {
    pub noise_pubkey: String,
    pub signing_pubkey: String,
}

#[derive(Debug, Serialize)]
pub struct MeshRegisterResponse {
    pub uin: String,
}

#[derive(Debug, Serialize)]
pub struct MeshLookupResponse {
    pub uin: String,
    pub online: bool,
    pub noise_pubkey: String,
}

#[derive(Debug, Deserialize)]
pub struct MeshSendRequest {
    pub from_uin: String,
    pub to_uin: String,
    pub ciphertext: String,
    pub ts: i64,
    pub sig: String,
}

#[derive(Debug, Deserialize)]
pub struct MeshPullRequest {
    pub uin: String,
    pub ts: i64,
    pub sig: String,
}

#[derive(Debug, Serialize)]
pub struct MeshMessage {
    pub from_uin: String,
    pub ciphertext: String,
}

#[derive(Debug, Serialize)]
pub struct MeshPullResponse {
    pub messages: Vec<MeshMessage>,
}

fn mesh_error(err: MeshError) -> warp::reply::Response {
    match err {
        MeshError::BadInput(msg) => http::bad_request(&msg).into_response(),
        MeshError::UnknownUin => http::not_found("mesh_uin_not_found").into_response(),
        MeshError::BadSignature | MeshError::BadTimestamp => warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 403,
                "message": "mesh_auth_failed"
            })),
            warp::http::StatusCode::FORBIDDEN,
        )
        .into_response(),
        MeshError::InboxFull => warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 429,
                "message": "mesh_inbox_full"
            })),
            warp::http::StatusCode::TOO_MANY_REQUESTS,
        )
        .into_response(),
    }
}

fn too_many_requests() -> warp::reply::Response {
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"status": 429, "message": "Too many requests"})),
        warp::http::StatusCode::TOO_MANY_REQUESTS,
    )
    .into_response()
}

fn check_rate_limit(
    limiter: &RateLimiter,
    remote: Option<SocketAddr>,
    x_forwarded_for: Option<&str>,
) -> Result<(), warp::reply::Response> {
    if limiter.check(client_ip(remote, x_forwarded_for)) {
        Ok(())
    } else {
        Err(too_many_requests())
    }
}

// POST /v1/mesh/register — idempotent per noise_pubkey.
pub async fn mesh_register_handler(
    req: MeshRegisterRequest,
    mesh: Arc<MeshRegistry>,
    remote: Option<SocketAddr>,
    x_forwarded_for: Option<String>,
    rate_limiter: Arc<RateLimiter>,
) -> Result<impl warp::Reply, warp::Rejection> {
    if let Err(resp) = check_rate_limit(&rate_limiter, remote, x_forwarded_for.as_deref()) {
        return Ok(resp);
    }

    let noise = match fcore::mesh::normalize_pubkey_hex(&req.noise_pubkey) {
        Ok(v) => v,
        Err(e) => return Ok(mesh_error(e)),
    };
    let signing = match fcore::mesh::normalize_pubkey_hex(&req.signing_pubkey) {
        Ok(v) => v,
        Err(e) => return Ok(mesh_error(e)),
    };

    let identity = mesh.register(&noise, &signing);
    tracing::debug!("mesh: register uin {}", identity.uin);

    Ok(warp::reply::json(&MeshRegisterResponse { uin: identity.uin }).into_response())
}

// GET /v1/mesh/lookup/{uin} — no auth.
pub async fn mesh_lookup_handler(
    uin: String,
    mesh: Arc<MeshRegistry>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let Some(identity) = mesh.lookup(&uin) else {
        return Ok(http::not_found("mesh_uin_not_found").into_response());
    };

    Ok(warp::reply::json(&MeshLookupResponse {
        uin: identity.uin.clone(),
        online: MeshRegistry::is_online(&identity),
        noise_pubkey: identity.noise_pubkey,
    })
    .into_response())
}

// POST /v1/mesh/send — sealed slot for the recipient.
pub async fn mesh_send_handler(
    req: MeshSendRequest,
    mesh: Arc<MeshRegistry>,
    remote: Option<SocketAddr>,
    x_forwarded_for: Option<String>,
    rate_limiter: Arc<RateLimiter>,
) -> Result<impl warp::Reply, warp::Rejection> {
    if let Err(resp) = check_rate_limit(&rate_limiter, remote, x_forwarded_for.as_deref()) {
        return Ok(resp);
    }

    match mesh.send(
        &req.from_uin,
        &req.to_uin,
        &req.ciphertext,
        req.ts,
        &req.sig,
    ) {
        Ok(()) => Ok(warp::reply::json(&serde_json::json!({"status": "ok"})).into_response()),
        Err(e) => Ok(mesh_error(e)),
    }
}

// POST /v1/mesh/pull — drains the caller's inbox.
pub async fn mesh_pull_handler(
    req: MeshPullRequest,
    mesh: Arc<MeshRegistry>,
    remote: Option<SocketAddr>,
    x_forwarded_for: Option<String>,
    rate_limiter: Arc<RateLimiter>,
) -> Result<impl warp::Reply, warp::Rejection> {
    if let Err(resp) = check_rate_limit(&rate_limiter, remote, x_forwarded_for.as_deref()) {
        return Ok(resp);
    }

    match mesh.pull(&req.uin, req.ts, &req.sig) {
        Ok(slots) => Ok(warp::reply::json(&MeshPullResponse {
            messages: slots
                .into_iter()
                .map(|s| MeshMessage {
                    from_uin: s.from_uin,
                    ciphertext: s.ciphertext,
                })
                .collect(),
        })
        .into_response()),
        Err(e) => Ok(mesh_error(e)),
    }
}
