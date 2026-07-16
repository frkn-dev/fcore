use crate::config::Config;
use crate::payment_client;
use crate::postgres::{PartnerRow, PgContext};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use uuid::Uuid;
use warp::Reply;

#[derive(Clone)]
pub struct PartnerState {
    pub pg: PgContext,
    pub config: Config,
    pub http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct PartnerLoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct CreatePartnerRequest {
    pub email: String,
    pub password: String,
    pub name: String,
    #[serde(default)]
    pub share_percent: f64,
    #[serde(default)]
    pub show_share: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreatePartnerPromocodeRequest {
    pub code: String,
    #[serde(default)]
    pub discount_percent: i32,
    pub max_uses: Option<i32>,
    pub duration_days: Option<i32>,
    pub expires_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct AttachPartnerPromocodeRequest {
    pub partner_id: Uuid,
    pub code: String,
    pub payment_promocode_id: Uuid,
    #[serde(default)]
    pub discount_percent: i32,
    pub max_uses: Option<i32>,
    pub duration_days: Option<i32>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize)]
pub struct PartnerMeResponse {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub share_percent: f64,
    pub show_share: bool,
}

#[derive(Serialize)]
pub struct PartnerPromocodeResponse {
    pub id: Uuid,
    pub code: String,
    pub discount_percent: i32,
    pub max_uses: Option<i32>,
    pub duration_days: Option<i32>,
    pub expires_at: Option<chrono::DateTime<Utc>>,
    pub active: bool,
    pub created_at: chrono::DateTime<Utc>,
}

impl From<PartnerRow> for PartnerMeResponse {
    fn from(p: PartnerRow) -> Self {
        Self {
            id: p.id,
            email: p.email,
            name: p.name,
            share_percent: p.share_percent,
            show_share: p.show_share,
        }
    }
}

impl From<crate::postgres::PartnerPromocodeRow> for PartnerPromocodeResponse {
    fn from(p: crate::postgres::PartnerPromocodeRow) -> Self {
        Self {
            id: p.id,
            code: p.code,
            discount_percent: p.discount_percent,
            max_uses: p.max_uses,
            duration_days: p.duration_days,
            expires_at: p.expires_at,
            active: p.active,
            created_at: p.created_at,
        }
    }
}

fn bad_request(msg: &str) -> Box<dyn Reply + Send> {
    Box::new(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"success": false, "message": msg}),
        ),
        warp::http::StatusCode::BAD_REQUEST,
    ))
}

fn unauthorized() -> Box<dyn Reply + Send> {
    Box::new(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"success": false, "message": "Unauthorized"}),
        ),
        warp::http::StatusCode::UNAUTHORIZED,
    ))
}

fn json_ok(body: serde_json::Value) -> Box<dyn Reply + Send> {
    Box::new(warp::reply::json(&body))
}

pub async fn create_partner_handler(
    state: PartnerState,
    req: CreatePartnerRequest,
) -> Result<Box<dyn Reply + Send>, Infallible> {
    let email = req.email.trim().to_lowercase();
    if email.is_empty() {
        return Ok(bad_request("email is required"));
    }
    if req.password.len() < 6 {
        return Ok(bad_request("password must be at least 6 characters"));
    }
    if req.name.trim().is_empty() {
        return Ok(bad_request("name is required"));
    }

    match state
        .pg
        .partners()
        .create(&email, &req.password, &req.name, req.share_percent, req.show_share)
        .await
    {
        Ok(id) => Ok(json_ok(serde_json::json!({
            "success": true,
            "id": id,
        }))),
        Err(e) => {
            tracing::error!("Failed to create partner: {}", e);
            Ok(bad_request("Failed to create partner"))
        }
    }
}

pub async fn login_handler(
    state: PartnerState,
    req: PartnerLoginRequest,
) -> Result<Box<dyn Reply + Send>, Infallible> {
    let email = req.email.trim().to_lowercase();
    if email.is_empty() || req.password.is_empty() {
        return Ok(bad_request("email and password are required"));
    }

    let partner = match state.pg.partners().verify_password(&email, &req.password).await {
        Ok(Some(p)) => p,
        Ok(None) => return Ok(unauthorized()),
        Err(e) => {
            tracing::error!("Failed to verify password: {}", e);
            return Ok(bad_request("Login failed"));
        }
    };

    if !partner.active {
        return Ok(bad_request("Account is disabled"));
    }

    let ttl = state.config.partner.session_ttl_hours;
    let token = match state.pg.sessions().create(partner.id, ttl).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Failed to create session: {}", e);
            return Ok(bad_request("Login failed"));
        }
    };

    Ok(json_ok(serde_json::json!({
        "success": true,
        "token": token,
        "partner": PartnerMeResponse::from(partner),
    })))
}

pub async fn me_handler(
    _state: PartnerState,
    partner: PartnerRow,
) -> Result<Box<dyn Reply + Send>, Infallible> {
    Ok(json_ok(serde_json::json!({
        "success": true,
        "partner": PartnerMeResponse::from(partner),
    })))
}

pub async fn create_promocode_handler(
    state: PartnerState,
    partner: PartnerRow,
    req: CreatePartnerPromocodeRequest,
) -> Result<Box<dyn Reply + Send>, Infallible> {
    let code = req.code.trim().to_uppercase();
    if code.is_empty() {
        return Ok(bad_request("code is required"));
    }

    let dashboard_id = match state
        .pg
        .promocodes()
        .create(
            partner.id,
            &code,
            req.discount_percent,
            req.max_uses,
            req.duration_days,
            req.expires_at,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("Failed to create promocode in dashboard: {}", e);
            return Ok(bad_request("Failed to create promocode"));
        }
    };

    let Some(admin_token) = &state.config.payment.admin_token else {
        return Ok(json_ok(serde_json::json!({
            "success": true,
            "id": dashboard_id,
        })));
    };

    let payment_req = payment_client::CreatePaymentPromocodeRequest {
        code: code.clone(),
        discount_percent: req.discount_percent,
        max_uses: req.max_uses,
        duration_days: req.duration_days,
        expires_at: req.expires_at,
        partner_id: partner.id,
    };

    match payment_client::create_promocode(
        &state.http,
        &state.config.payment.endpoint,
        admin_token,
        payment_req,
    )
    .await
    {
        Ok(payment_id) => {
            if let Err(e) = state
                .pg
                .promocodes()
                .set_payment_id(dashboard_id, partner.id, payment_id)
                .await
            {
                tracing::error!(
                    "Created payment promocode {} but failed to link dashboard promocode {}: {}",
                    payment_id,
                    dashboard_id,
                    e
                );
            }
            Ok(json_ok(serde_json::json!({
                "success": true,
                "id": dashboard_id,
            })))
        }
        Err(e) => {
            tracing::error!(
                "Payment gateway refused promocode {} for partner {}: {}",
                code,
                partner.id,
                e
            );
            if let Err(e) = state.pg.promocodes().delete(dashboard_id, partner.id).await {
                tracing::error!(
                    "Failed to rollback dashboard promocode {} after payment error: {}",
                    dashboard_id,
                    e
                );
            }
            Ok(bad_request("Payment gateway rejected promocode"))
        }
    }
}

pub async fn list_promocodes_handler(
    state: PartnerState,
    partner: PartnerRow,
) -> Result<Box<dyn Reply + Send>, Infallible> {
    match state.pg.promocodes().list(partner.id).await {
        Ok(rows) => {
            let promos: Vec<PartnerPromocodeResponse> = rows.into_iter().map(|r| r.into()).collect();
            Ok(json_ok(serde_json::json!({
                "success": true,
                "promocodes": promos,
            })))
        }
        Err(e) => {
            tracing::error!("Failed to list promocodes: {}", e);
            Ok(bad_request("Failed to list promocodes"))
        }
    }
}

pub async fn delete_promocode_handler(
    state: PartnerState,
    partner: PartnerRow,
    id: Uuid,
) -> Result<Box<dyn Reply + Send>, Infallible> {
    let promocode = match state.pg.promocodes().find_by_id(id, partner.id).await {
        Ok(Some(p)) => p,
        Ok(None) => return Ok(bad_request("Promocode not found")),
        Err(e) => {
            tracing::error!("Failed to find promocode: {}", e);
            return Ok(bad_request("Failed to delete promocode"));
        }
    };

    if let (Some(admin_token), Some(payment_id)) = (
        &state.config.payment.admin_token,
        promocode.payment_promocode_id,
    ) {
        if let Err(e) = payment_client::delete_promocode(
            &state.http,
            &state.config.payment.endpoint,
            admin_token,
            payment_id,
        )
        .await
        {
            tracing::error!(
                "Failed to delete payment promocode {} for dashboard promocode {}: {}",
                payment_id,
                id,
                e
            );
            return Ok(bad_request("Failed to delete promocode in payment gateway"));
        }
    }

    match state.pg.promocodes().delete(id, partner.id).await {
        Ok(0) => Ok(bad_request("Promocode not found")),
        Ok(_) => Ok(json_ok(serde_json::json!({"success": true}))),
        Err(e) => {
            tracing::error!("Failed to delete promocode: {}", e);
            Ok(bad_request("Failed to delete promocode"))
        }
    }
}

pub async fn attach_promocode_handler(
    state: PartnerState,
    req: AttachPartnerPromocodeRequest,
) -> Result<Box<dyn Reply + Send>, Infallible> {
    let code = req.code.trim().to_uppercase();
    if code.is_empty() {
        return Ok(bad_request("code is required"));
    }

    match state
        .pg
        .promocodes()
        .attach(
            req.partner_id,
            &code,
            req.payment_promocode_id,
            req.discount_percent,
            req.max_uses,
            req.duration_days,
            req.expires_at,
        )
        .await
    {
        Ok(id) => Ok(json_ok(serde_json::json!({
            "success": true,
            "id": id,
        }))),
        Err(e) => {
            tracing::error!("Failed to attach promocode: {}", e);
            Ok(bad_request("Failed to attach promocode"))
        }
    }
}
