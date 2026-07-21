use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct CreatePaymentPromocodeRequest {
    pub code: String,
    pub discount_percent: i32,
    pub max_uses: Option<i32>,
    pub duration_days: Option<i32>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub partner_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct CreatePaymentPromocodeResponse {
    pub id: Uuid,
}

#[derive(Debug, thiserror::Error)]
pub enum PaymentClientError {
    #[error("payment gateway request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("payment gateway returned {status}: {message}")]
    Api { status: reqwest::StatusCode, message: String },
}

pub async fn create_promocode(
    http: &reqwest::Client,
    endpoint: &str,
    token: &str,
    req: CreatePaymentPromocodeRequest,
) -> std::result::Result<Uuid, PaymentClientError> {
    let url = format!("{}/api/partner/promocodes", endpoint.trim_end_matches('/'));
    let resp = http
        .post(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .json(&req)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let message = resp.text().await.unwrap_or_default();
        return Err(PaymentClientError::Api { status, message });
    }

    let body: CreatePaymentPromocodeResponse = resp.json().await?;
    Ok(body.id)
}

pub async fn delete_promocode(
    http: &reqwest::Client,
    endpoint: &str,
    token: &str,
    payment_promocode_id: Uuid,
) -> std::result::Result<(), PaymentClientError> {
    let url = format!(
        "{}/api/partner/promocodes/{}",
        endpoint.trim_end_matches('/'),
        payment_promocode_id
    );
    let resp = http
        .delete(&url)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() && status != reqwest::StatusCode::NOT_FOUND {
        let message = resp.text().await.unwrap_or_default();
        return Err(PaymentClientError::Api { status, message });
    }

    Ok(())
}
