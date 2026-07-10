use crate::aggregator::MetricSample;
use std::time::Duration;

#[derive(Clone)]
pub struct MetricSender {
    client: reqwest::Client,
    endpoint: String,
    token: String,
}

#[derive(serde::Serialize)]
struct IngestRequest {
    samples: Vec<ApiMetricSample>,
}

#[derive(serde::Serialize)]
struct ApiMetricSample {
    pub name: String,
    pub tags: std::collections::BTreeMap<String, String>,
    pub value: f64,
    pub timestamp_ms: i64,
}

impl MetricSender {
    pub fn new(endpoint: String, token: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            endpoint,
            token,
        }
    }

    pub async fn send(
        &self,
        samples: &[MetricSample],
    ) -> Result<usize, reqwest::Error> {
        if samples.is_empty() {
            return Ok(0);
        }

        let body = IngestRequest {
            samples: samples
                .iter()
                .map(|s| ApiMetricSample {
                    name: s.name.clone(),
                    tags: s.tags.clone(),
                    value: s.value,
                    timestamp_ms: s.timestamp_ms,
                })
                .collect(),
        };

        let response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.token))
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            tracing::error!("Metrics ingest failed: {} - {}", status, text);
        }

        Ok(samples.len())
    }
}
