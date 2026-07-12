use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::Deserialize;
use tracing::error;
use uuid::Uuid;

use fcore::{Env, Tag};

use super::config::ApiConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct SubscriptionInfo {
    pub id: Uuid,
    pub refer_code: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct ApiClient {
    client: reqwest::Client,
    endpoint: String,
    token: String,
}

impl ApiClient {
    pub fn new(config: &ApiConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: config.endpoint.trim_end_matches('/').to_string(),
            token: config.token.clone(),
        }
    }

    fn auth_headers(&self, trace_id: Option<Uuid>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let token = format!("Bearer {}", self.token);
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&token).expect("invalid api token"),
        );
        if let Some(trace_id) = trace_id {
            if let Ok(v) = HeaderValue::from_str(&trace_id.to_string()) {
                headers.insert("X-Trace-Id", v);
            }
        }
        headers
    }

    pub async fn create_subscription(
        &self,
        days: Option<i64>,
        limit_bytes: Option<i64>,
        trace_id: Option<Uuid>,
    ) -> anyhow::Result<SubscriptionInfo> {
        let body = serde_json::json!({
            "days": days,
            "limit_bytes": limit_bytes,
        });

        let resp = self
            .client
            .post(format!("{}/subscription", self.endpoint))
            .headers(self.auth_headers(trace_id))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("API returned {}: {}", status, text);
        }

        let parsed: ApiResponse<ApiInstance> = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("Failed to parse subscription response: {e}\n{text}"))?;

        match parsed.response.instance {
            ApiInstance::Subscription(s) => Ok(SubscriptionInfo {
                id: parsed.response.id,
                refer_code: s.refer_code,
                expires_at: s.expires_at,
            }),
            _ => anyhow::bail!("Unexpected API instance type"),
        }
    }

    pub async fn create_connection(
        &self,
        env: &Env,
        subscription_id: Uuid,
        proto: &Tag,
        trace_id: Option<Uuid>,
    ) -> anyhow::Result<Uuid> {
        let body = serde_json::json!({
            "env": env,
            "subscription_id": subscription_id,
            "proto": proto,
        });

        let resp = self
            .client
            .post(format!("{}/connection", self.endpoint))
            .headers(self.auth_headers(trace_id))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            error!("Failed to create connection for {:?}/{}: {} {}", env, proto, status, text);
            anyhow::bail!("API returned {}: {}", status, text);
        }

        let parsed: ApiResponse<ApiInstance> = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("Failed to parse connection response: {e}\n{text}"))?;

        match parsed.response.instance {
            ApiInstance::Connection(c) => Ok(c.id),
            _ => {
                error!("Unexpected API instance type when creating connection");
                anyhow::bail!("Unexpected API instance type")
            }
        }
    }

    pub async fn get_subscription(
        &self,
        subscription_id: Uuid,
    ) -> anyhow::Result<SubscriptionInfo> {
        let resp = self
            .client
            .get(format!("{}/subscription/{}", self.endpoint, subscription_id))
            .headers(self.auth_headers(None))
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("API returned {}: {}", status, text);
        }

        let info: SubscriptionInfo = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("Failed to parse subscription info: {e}\n{text}"))?;
        Ok(info)
    }
}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    #[allow(dead_code)]
    status: u16,
    #[allow(dead_code)]
    message: String,
    response: InstanceWithId<T>,
}

#[derive(Debug, Deserialize)]
struct InstanceWithId<T> {
    id: Uuid,
    instance: T,
}

#[derive(Debug, Deserialize)]
enum ApiInstance {
    Subscription(ApiSubscription),
    Connection(ApiConnection),
}

#[derive(Debug, Deserialize)]
struct ApiSubscription {
    #[allow(dead_code)]
    id: Uuid,
    refer_code: String,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct ApiConnection {
    id: Uuid,
}
