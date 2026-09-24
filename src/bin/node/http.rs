use async_trait::async_trait;
use reqwest::{Client as HttpClient, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::Ipv4Addr;

use fcore::{
    http::{
        request::ConnType,
        response::{Instance, InstanceWithId, ResponseMessage},
    },
    ConnectionBaseOperations, Env, Error, Inbound, NodeType, Result, Tag, Topic,
};

use crate::node::Node;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeRequest {
    pub env: Env,
    pub hostname: String,
    pub address: Ipv4Addr,
    pub inbounds: HashMap<Tag, Inbound>,
    pub uuid: uuid::Uuid,
    pub label: String,
    pub interface: String,
    pub cores: usize,
    pub max_bandwidth_bps: i64,
    pub country: String,
    pub r#type: NodeType,
    pub cluster: Option<String>,
    /// Extra entry IPs (node_ips[0] == address); None = single-address
    /// node. Old api binaries ignore the unknown field.
    #[serde(default)]
    pub node_ips: Option<Vec<Ipv4Addr>>,
}

#[async_trait]
pub trait ApiRequests {
    async fn register_node(&self, _endpoint: String, _token: String) -> Result<()>;
    async fn sync_connections(
        &self,
        endpoint: String,
        token: String,
        proto: Tag,
        last_update: Option<u64>,
    ) -> Result<()>;
}

#[async_trait]
impl<C> ApiRequests for Node<C>
where
    C: ConnectionBaseOperations + Send + Sync + Clone + 'static,
{
    async fn sync_connections(
        &self,
        endpoint: String,
        token: String,
        proto: Tag,
        last_update: Option<u64>,
    ) -> Result<()> {
        let topic = Topic::Init(self.node.uuid);
        let env = self.node.env.clone();

        let req = ConnType {
            proto,
            last_update,
            env: env.clone(),
            topic: topic.clone(),
        };

        let mut endpoint_url = Url::parse(&endpoint).map_err(|e| {
            tracing::error!("Failed to parse endpoint URL '{}': {}", endpoint, e);
            Error::Custom("Invalid API endpoint".to_string())
        })?;

        endpoint_url
            .path_segments_mut()
            .map_err(|_| Error::Custom("Invalid API endpoint".to_string()))?
            .push("connections")
            .push("sync");

        let endpoint_str = endpoint_url.to_string();

        tracing::debug!("POST /connections/sync Body: {:?}", req);

        let token = load_runtime_token(&token);
        let res = HttpClient::new()
            .post(&endpoint_str)
            .header("Authorization", format!("Bearer {}", token))
            .json(&req)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("CRITICAL: reqwest send error: {:?}", e);
                Error::Custom(format!("HTTP Send Error: {}", e))
            })?;

        let status = res.status();
        let body = res.text().await?;

        if status.is_success() {
            let result: ResponseMessage<InstanceWithId<Instance>> = serde_json::from_str(&body)?;
            let count = match result.response.instance {
                Instance::Count(count) => count,
                _ => return Err(Error::Custom("Unexpected instance type".into())),
            };
            tracing::debug!(
                "Success: {} connections synced for {} - {} - {}",
                count,
                topic,
                env,
                proto
            );
            Ok(())
        } else if status == StatusCode::NOT_MODIFIED {
            tracing::debug!("No updates (304) for {} {} {}", topic.clone(), env, proto);
            Ok(())
        } else {
            tracing::error!("Request failed: {} - {}", status, body);
            Err(Error::Custom(format!("Status {}: {}", status, body)))
        }
    }

    async fn register_node(&self, endpoint: String, token: String) -> Result<()> {
        let token = load_runtime_token(&token);
        let node = self.node.clone();

        let mut endpoint_url = Url::parse(&endpoint)?;
        {
            let mut segs = endpoint_url
                .path_segments_mut()
                .map_err(|_| Error::Custom("Invalid API endpoint".to_string()))?;
            if token.starts_with("inst_") {
                segs.push("private");
                segs.push("nodes");
            } else {
                segs.push("node");
            }
        }
        let endpoint_str = endpoint_url.to_string();

        match serde_json::to_string_pretty(&node) {
            Ok(json) => tracing::debug!("Serialized node for environment '{}': {}", node.env, json),
            Err(e) => tracing::error!("Error serializing node '{}': {}", node.hostname, e),
        }

        let node_request = NodeRequest {
            env: node.env.clone(),
            hostname: node.hostname.clone(),
            address: node.address,
            inbounds: node.inbounds.clone(),
            uuid: node.uuid,
            label: node.label.clone(),
            interface: node.interface.clone(),
            cores: node.cores,
            max_bandwidth_bps: node.max_bandwidth_bps,
            country: node.country,
            r#type: node.r#type,
            cluster: node.cluster.clone(),
            node_ips: node.node_ips.clone(),
        };

        if token.starts_with("node_") {
            tracing::debug!("Skipping register: durable node_ token present");
            return Ok(());
        }

        let res = HttpClient::new()
            .post(&endpoint_str)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", token))
            .json(&node_request)
            .send()
            .await?;

        let status = res.status();
        let body = res.text().await?;
        if status.is_success() || status == StatusCode::NOT_MODIFIED {
            tracing::debug!("Node is already registered: {:?}", status);
            if token.starts_with("inst_") {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                    if let Some(nt) = v
                        .pointer("/response/node_token")
                        .and_then(|x| x.as_str())
                        .or_else(|| {
                            v.pointer("/response/token")
                                .and_then(|x| x.as_str())
                        })
                    {
                        if let Err(e) = persist_node_token(nt) {
                            tracing::error!("Failed to persist node_token: {}", e);
                        } else {
                            tracing::info!("Persisted durable node_token for later sync");
                        }
                    }
                }
            }
            Ok(())
        } else {
            tracing::error!("Registration failed: {} - {}", status, body);
            Err(Error::Custom(format!(
                "Registration failed: {} - {}",
                status, body
            )))
        }
    }
}

fn runtime_token_path() -> std::path::PathBuf {
    std::path::PathBuf::from("api.token")
}

fn load_runtime_token(config_token: &str) -> String {
    let path = runtime_token_path();
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim();
        if t.starts_with("node_") {
            return t.to_string();
        }
    }
    config_token.trim().to_string()
}

fn persist_node_token(token: &str) -> std::io::Result<()> {
    std::fs::write(runtime_token_path(), token.trim())
}
