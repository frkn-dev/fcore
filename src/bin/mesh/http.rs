use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use warp::Reply;

use fcore::http::helpers as http;

use super::store::{Envelope, MeshError, Store};

const MAX_BODY_BYTES: u64 = 1024 * 1024;

pub fn body_limit() -> impl warp::Filter<Extract = (), Error = warp::Rejection> + Clone {
    warp::body::content_length_limit(MAX_BODY_BYTES)
}

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

pub fn client_ip(remote: Option<SocketAddr>, x_forwarded_for: Option<&str>) -> IpAddr {
    x_forwarded_for
        .and_then(|v| v.split(',').next()?.trim().parse::<IpAddr>().ok())
        .or_else(|| remote.map(|a| a.ip()))
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

fn too_many() -> warp::reply::Response {
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"status": 429, "message": "Too many requests"})),
        warp::http::StatusCode::TOO_MANY_REQUESTS,
    )
    .into_response()
}

pub fn limit(
    limiter: &RateLimiter,
    remote: Option<SocketAddr>,
    x_forwarded_for: Option<&str>,
) -> Result<(), warp::reply::Response> {
    if limiter.check(client_ip(remote, x_forwarded_for)) {
        Ok(())
    } else {
        Err(too_many())
    }
}

fn mesh_error(err: MeshError) -> warp::reply::Response {
    match err {
        MeshError::BadInput(msg) => http::bad_request(&msg).into_response(),
        MeshError::UnknownUin => http::not_found("mesh_uin_not_found").into_response(),
        MeshError::BadSignature | MeshError::BadTimestamp | MeshError::SubscriptionRejected => {
            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"status": 403, "message": "mesh_auth_failed"})),
                warp::http::StatusCode::FORBIDDEN,
            )
            .into_response()
        }
        MeshError::Conflict => http::conflict("mesh_subscription_conflict").into_response(),
        MeshError::InboxFull => warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"status": 429, "message": "mesh_inbox_full"})),
            warp::http::StatusCode::TOO_MANY_REQUESTS,
        )
        .into_response(),
        MeshError::Internal(msg) => {
            tracing::error!(error = %msg, "mesh: internal");
            http::internal_error("mesh_internal").into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub subscription_id: String,
    pub subscription_secret: String,
    pub noise_pubkey: String,
    pub signing_pubkey: String,
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pub uin: String,
}

#[derive(Debug, Serialize)]
pub struct DeviceJson {
    pub noise_pubkey: String,
}

#[derive(Debug, Serialize)]
pub struct LookupResponse {
    pub uin: String,
    pub name: String,
    pub online: bool,
    pub devices: Vec<DeviceJson>,
}

#[derive(Debug, Deserialize)]
pub struct EnvelopeJson {
    pub device_pubkey: String,
    pub ciphertext: String,
}

#[derive(Debug, Deserialize)]
pub struct SendRequest {
    pub from_uin: String,
    pub to_uin: String,
    pub envelopes: Vec<EnvelopeJson>,
    pub ts: i64,
    pub sig: String,
}

#[derive(Debug, Deserialize)]
pub struct SignedRequest {
    pub uin: String,
    pub ts: i64,
    pub sig: String,
}

#[derive(Debug, Deserialize)]
pub struct NameRequest {
    pub uin: String,
    pub name: String,
    pub ts: i64,
    pub sig: String,
}

#[derive(Debug, Serialize)]
pub struct MessageJson {
    pub from_uin: String,
    pub ciphertext: String,
}

#[derive(Debug, Serialize)]
pub struct PullResponse {
    pub messages: Vec<MessageJson>,
}

pub async fn register(
    req: RegisterRequest,
    store: Arc<Store>,
    remote: Option<SocketAddr>,
    xff: Option<String>,
    limiter: Arc<RateLimiter>,
) -> Result<impl warp::Reply, warp::Rejection> {
    if let Err(resp) = limit(&limiter, remote, xff.as_deref()) {
        return Ok(resp);
    }
    match store
        .register(
            &req.subscription_id,
            &req.subscription_secret,
            &req.noise_pubkey,
            &req.signing_pubkey,
            req.name.as_deref(),
        )
        .await
    {
        Ok(uin) => Ok(warp::reply::json(&RegisterResponse { uin }).into_response()),
        Err(e) => Ok(mesh_error(e)),
    }
}

pub async fn lookup(uin: String, store: Arc<Store>) -> Result<impl warp::Reply, warp::Rejection> {
    match store.lookup(&uin) {
        Ok(found) => Ok(warp::reply::json(&LookupResponse {
            uin: found.uin,
            name: found.name,
            online: found.online,
            devices: found
                .devices
                .into_iter()
                .map(|d| DeviceJson {
                    noise_pubkey: d.noise_pubkey,
                })
                .collect(),
        })
        .into_response()),
        Err(e) => Ok(mesh_error(e)),
    }
}

pub async fn send(
    req: SendRequest,
    store: Arc<Store>,
    remote: Option<SocketAddr>,
    xff: Option<String>,
    limiter: Arc<RateLimiter>,
) -> Result<impl warp::Reply, warp::Rejection> {
    if let Err(resp) = limit(&limiter, remote, xff.as_deref()) {
        return Ok(resp);
    }
    let envelopes: Vec<Envelope> = req
        .envelopes
        .into_iter()
        .map(|e| Envelope {
            device_pubkey: e.device_pubkey,
            ciphertext: e.ciphertext,
        })
        .collect();
    match store.send(&req.from_uin, &req.to_uin, &envelopes, req.ts, &req.sig) {
        Ok(()) => Ok(warp::reply::json(&serde_json::json!({"status": "ok"})).into_response()),
        Err(e) => Ok(mesh_error(e)),
    }
}

pub async fn pull(
    req: SignedRequest,
    store: Arc<Store>,
    remote: Option<SocketAddr>,
    xff: Option<String>,
    limiter: Arc<RateLimiter>,
) -> Result<impl warp::Reply, warp::Rejection> {
    if let Err(resp) = limit(&limiter, remote, xff.as_deref()) {
        return Ok(resp);
    }
    match store.pull(&req.uin, req.ts, &req.sig) {
        Ok(slots) => Ok(messages(slots)),
        Err(e) => Ok(mesh_error(e)),
    }
}

pub async fn poll(
    req: SignedRequest,
    store: Arc<Store>,
    remote: Option<SocketAddr>,
    xff: Option<String>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let _ = (remote, xff);
    match store.poll(&req.uin, req.ts, &req.sig).await {
        Ok(slots) => Ok(messages(slots)),
        Err(e) => Ok(mesh_error(e)),
    }
}

pub async fn name(
    req: NameRequest,
    store: Arc<Store>,
    remote: Option<SocketAddr>,
    xff: Option<String>,
    limiter: Arc<RateLimiter>,
) -> Result<impl warp::Reply, warp::Rejection> {
    if let Err(resp) = limit(&limiter, remote, xff.as_deref()) {
        return Ok(resp);
    }
    match store.set_name(&req.uin, &req.name, req.ts, &req.sig) {
        Ok(()) => Ok(warp::reply::json(&serde_json::json!({"status": "ok"})).into_response()),
        Err(e) => Ok(mesh_error(e)),
    }
}

fn messages(slots: Vec<super::store::SlotView>) -> warp::reply::Response {
    warp::reply::json(&PullResponse {
        messages: slots
            .into_iter()
            .map(|s| MessageJson {
                from_uin: s.from_uin,
                ciphertext: s.ciphertext,
            })
            .collect(),
    })
    .into_response()
}
