use tracing::{error, warn};

use super::config::MrktingConfig;

/// Fire-and-forget lite enforcement event to mrkting
/// (`POST {endpoint}/events/lite`). The endpoint may not exist yet — a
/// non-success status is logged at warn and never retried; failures are
/// never fatal to the caller.
pub async fn send_lite_event(mrkting: &MrktingConfig, event: serde_json::Value) {
    let client = reqwest::Client::new();
    let url = format!("{}/events/lite", mrkting.endpoint.trim_end_matches('/'));

    match client
        .post(&url)
        .header("Authorization", format!("Bearer {}", mrkting.token))
        .header("Content-Type", "application/json")
        .json(&event)
        .send()
        .await
    {
        Ok(resp) => {
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                warn!("mrkting lite event call failed: {} {}", status, text);
            }
        }
        Err(err) => {
            error!("Failed to call mrkting lite event: {}", err);
        }
    }
}
