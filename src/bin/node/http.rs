use async_trait::async_trait;
use reqwest::{Client as HttpClient, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::net::Ipv4Addr;

use fcore::{
    http::{
        request::ConnType,
        response::{Instance, InstanceWithId, ResponseMessage},
    },
    ConnectionBaseOperations, Endpoint, Env, Error, Inbound, NodeType, Result, Tag, Topic,
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
    /// Entry points of this node in the ONE `[[node.endpoints]]` shape
    /// (`ip` + optional `port`/`weight`/`label`). The api stores and publishes
    /// exactly this list; `node_ips`/`node_weights` below are the DEPRECATED
    /// mirror an old api build still reads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<Endpoint>,
    /// `true` when this node's config still declares the DEPRECATED
    /// `node_ips`/`node_weights` keys. Counted by the api (`/status`) so the
    /// legacy path is removed only once every node reports `false`.
    #[serde(default)]
    pub endpoints_legacy: bool,
    /// DEPRECATED — use `[[node.endpoints]]`.
    /// TODO(remove-legacy-endpoints): delete once every node config in
    /// `env/dev/servers` is migrated and no supported api build needs the key.
    /// Extra entry IPs (node_ips[0] == address); None = single-address
    /// node. Old api binaries ignore the unknown field.
    #[serde(default)]
    #[deprecated(
        note = "use [[node.endpoints]]; remove once no node reports endpoints_legacy"
    )]
    pub node_ips: Option<Vec<Ipv4Addr>>,
    /// Optional pool/group name (config.toml `[node] pool`). Old api
    /// binaries ignore the unknown field.
    #[serde(default)]
    pub pool: Option<String>,
    /// Optional per-language translations of `pool`.
    #[serde(default)]
    pub pool_translation: Option<BTreeMap<String, String>>,
    /// Country of the whole pool (config.toml `[node] pool_country_code`): the
    /// card's flag. Old api builds ignore the unknown field.
    #[serde(default)]
    pub pool_country_code: Option<String>,
    /// Human name of the pool country (`[node] pool_country_name`).
    #[serde(default)]
    pub pool_country_name: Option<String>,
    /// Per-entry weights (config.toml `[node] node_weights`), keyed by an
    /// address from `node_ips`. Old api builds ignore the unknown field.
    ///
    /// DEPRECATED — use `[[node.endpoints]]`.
    /// TODO(remove-legacy-endpoints): delete once every node config is migrated.
    #[serde(default)]
    #[deprecated(
        note = "use [[node.endpoints]]; remove once no node reports endpoints_legacy"
    )]
    pub node_weights: BTreeMap<String, i32>,
    /// Base URL this node serves its speed-test files from (config.toml
    /// `[node] bench_url`). Omitted when the node has no bench, so a
    /// re-registration never clears a value the api already stores. Old api
    /// builds ignore the unknown field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bench_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    /// The api published a list and reported this many connections.
    Applied { count: usize },
    /// A full sync returned 304 because the api has no connections for the tag.
    EmptyFull,
    /// A delta sync returned 304; it is not a full list and must never prune.
    NotModified,
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
    ) -> Result<SyncOutcome>;

    /// How many connections of `env` + each tag the api holds.
    ///
    /// Asked once at startup to decide whether the snapshot still describes
    /// reality, and once an hour to notice drift. The api answers from the same
    /// predicate it uses for `/connections/sync`.
    async fn connection_counts(
        &self,
        endpoint: String,
        token: String,
        env: fcore::Env,
        protos: Vec<Tag>,
    ) -> Result<std::collections::BTreeMap<String, u64>>;

    /// Tell the api that the local counters diverged from its own.
    async fn report_snapshot_drift(
        &self,
        endpoint: String,
        token: String,
        tag: Tag,
        local: u64,
        api: u64,
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
    ) -> Result<SyncOutcome> {
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

        // В теле sync лежат приватные ключи клиентов (wg_privkey), поэтому
        // печатаем только размер и число соединений: при level=debug журнал
        // иначе становится хранилищем ключей (так и случилось на self-hosted).
        tracing::debug!(
            "POST /connections/sync: proto {:?}, env {}, {} байт",
            req.proto,
            req.env,
            serde_json::to_string(&req).map(|s| s.len()).unwrap_or(0)
        );

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
            Ok(SyncOutcome::Applied { count })
        } else if status == StatusCode::NOT_MODIFIED {
            tracing::debug!("No updates (304) for {} {} {}", topic.clone(), env, proto);
            if last_update.is_none() {
                Ok(SyncOutcome::EmptyFull)
            } else {
                Ok(SyncOutcome::NotModified)
            }
        } else {
            tracing::error!("Request failed: {} - {}", status, body);
            Err(Error::Custom(format!("Status {}: {}", status, body)))
        }
    }

    async fn connection_counts(
        &self,
        endpoint: String,
        token: String,
        env: fcore::Env,
        protos: Vec<Tag>,
    ) -> Result<std::collections::BTreeMap<String, u64>> {
        let token = load_runtime_token(&token);
        let mut url = Url::parse(&endpoint)?;
        url.path_segments_mut()
            .map_err(|_| Error::Custom("Invalid API endpoint".to_string()))?
            .push("connections")
            .push("count");

        let body = serde_json::json!({ "env": env, "proto": protos });
        let res = HttpClient::new()
            .post(url.to_string())
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Custom(format!("count request failed: {}", e)))?;

        let status = res.status();
        let text = res.text().await?;
        if !status.is_success() {
            return Err(Error::Custom(format!(
                "count request: {} - {}",
                status, text
            )));
        }

        #[derive(serde::Deserialize)]
        struct CountResponse {
            response: Counts,
        }
        #[derive(serde::Deserialize)]
        struct Counts {
            counts: std::collections::BTreeMap<String, u64>,
        }

        let parsed: CountResponse = serde_json::from_str(&text)?;
        Ok(parsed.response.counts)
    }

    async fn report_snapshot_drift(
        &self,
        endpoint: String,
        token: String,
        tag: Tag,
        local: u64,
        api: u64,
    ) -> Result<()> {
        let token = load_runtime_token(&token);
        let mut url = Url::parse(&endpoint)?;
        {
            let mut segs = url
                .path_segments_mut()
                .map_err(|_| Error::Custom("Invalid API endpoint".to_string()))?;
            segs.push("node");
            segs.push("snapshot");
            segs.push("drift");
        }

        let body = serde_json::json!({
            "hostname": self.node.hostname,
            "env": self.node.env.to_string(),
            "tag": format!("{:?}", tag),
            "local": local,
            "api": api,
            "at": chrono::Utc::now().to_rfc3339(),
        });

        let res = HttpClient::new()
            .post(url.to_string())
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Custom(format!("drift report failed: {}", e)))?;

        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(Error::Custom(format!(
                "drift report: {} - {}",
                status, text
            )));
        }
        Ok(())
    }

    #[allow(deprecated)]
    async fn register_node(&self, endpoint: String, token: String) -> Result<()> {
        let token = load_runtime_token(&token);
        let node = self.node.clone();

        let mut endpoint_url = Url::parse(&endpoint)?;
        {
            let mut segs = endpoint_url
                .path_segments_mut()
                .map_err(|_| Error::Custom("Invalid API endpoint".to_string()))?;
            if token.starts_with("inst_") || token.starts_with("node_") {
                segs.push("private");
                segs.push("nodes");
            } else {
                segs.push("node");
            }
        }
        let endpoint_str = endpoint_url.to_string();

        // Раньше здесь печатался весь JSON ноды, включая инбаунды с
        // `mtproto_secret`. Оставляем сводку: размер и набор тегов.
        match serde_json::to_string(&node) {
            Ok(json) => tracing::debug!(
                "Serialized node for environment '{}': {} байт, инбаунды [{}]",
                node.env,
                json.len(),
                node.inbounds
                    .values()
                    .map(|i| i.tag.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
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
            endpoints: node.endpoints.clone(),
            endpoints_legacy: node.endpoints_legacy,
            node_ips: node.node_ips.clone(),
            pool: node.pool.clone(),
            pool_translation: node.pool_translation.clone(),
            pool_country_code: node.pool_country_code.clone(),
            pool_country_name: node.pool_country_name.clone(),
            node_weights: node.node_weights.clone(),
            bench_url: node.bench_url.clone(),
        };

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
                        .or_else(|| v.pointer("/response/token").and_then(|x| x.as_str()))
                    {
                        if let Err(e) = persist_node_token(nt) {
                            tracing::error!("Failed to persist node_token: {}", e);
                        } else {
                            tracing::info!("Persisted durable node_token for later sync");
                        }
                    }
                    if let Some(scope) = v.pointer("/response/scope_env").and_then(|x| x.as_str()) {
                        let toml_env = scope.strip_prefix("custom").unwrap_or(scope);
                        if let Err(e) = std::fs::write("scope.env", toml_env) {
                            tracing::error!("Failed to persist scope env: {}", e);
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

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::NodeRequest;
    use fcore::{Endpoint, Env, NodeType};
    use std::collections::HashMap;

    fn request(bench_url: Option<&str>) -> NodeRequest {
        NodeRequest {
            env: Env::Ru,
            hostname: "msk-tw.frkn.org".to_string(),
            address: "185.156.108.232".parse().unwrap(),
            inbounds: HashMap::new(),
            uuid: uuid::Uuid::new_v4(),
            label: "RU MSK".to_string(),
            interface: "eth0".to_string(),
            cores: 4,
            max_bandwidth_bps: 1_000_000_000,
            country: "RU".to_string(),
            r#type: NodeType::Node,
            cluster: None,
            endpoints: Vec::new(),
            endpoints_legacy: false,
            node_ips: None,
            pool: None,
            pool_translation: None,
            pool_country_code: None,
            pool_country_name: None,
            node_weights: Default::default(),
            bench_url: bench_url.map(str::to_string),
        }
    }

    // A node that declares its bench publishes it in the registration payload,
    // so the api can store it (`nodes.bench_url` -> `/status` `speedtest_url`).
    #[test]
    fn registration_carries_bench_url() {
        let json = serde_json::to_value(request(Some(
            "https://tw-ru-msk-bench-01.bench.beacon.frkn.org:19443",
        )))
        .unwrap();
        assert_eq!(
            json["bench_url"],
            "https://tw-ru-msk-bench-01.bench.beacon.frkn.org:19443"
        );
    }

    // Absent means "no bench here", not "clear the stored one": the key must
    // not appear at all, so an old api / another node cannot wipe the value.
    #[test]
    fn registration_omits_absent_bench_url() {
        let json = serde_json::to_value(request(None)).unwrap();
        assert!(
            json.get("bench_url").is_none(),
            "bench_url must be omitted, not sent as null: {json}"
        );
    }

    // The announcement carries the ONE endpoint shape: `ip` plus the optional
    // port/weight/label, together with the legacy mirror an old api reads and
    // the migration flag the new api counts.
    #[test]
    fn registration_carries_endpoints_and_legacy_flag() {
        let mut req = request(None);
        req.endpoints = vec![
            Endpoint {
                ip: "185.156.108.232".parse().unwrap(),
                port: Some(8443),
                weight: 200,
                label: Some("ru-01".to_string()),
            },
            Endpoint {
                ip: "185.156.108.233".parse().unwrap(),
                port: None,
                weight: 100,
                label: None,
            },
        ];
        req.endpoints_legacy = true;
        req.node_ips = Some(vec![
            "185.156.108.232".parse().unwrap(),
            "185.156.108.233".parse().unwrap(),
        ]);

        let json = serde_json::to_value(req).unwrap();
        assert_eq!(json["endpoints"][0]["ip"], "185.156.108.232");
        assert_eq!(json["endpoints"][0]["port"], 8443);
        assert_eq!(json["endpoints"][0]["weight"], 200);
        assert_eq!(json["endpoints"][0]["label"], "ru-01");
        assert_eq!(json["endpoints"][1]["weight"], 100);
        assert!(
            json["endpoints"][1].get("port").is_none(),
            "an endpoint without a port means 'the inbound port'"
        );
        assert_eq!(json["endpoints_legacy"], true);
        // Legacy mirror still travels, so an old api build keeps working.
        assert_eq!(json["node_ips"][1], "185.156.108.233");
    }

    // A single-address node sends no endpoints list at all (an empty one is
    // meaningless) — the api expands `address` into the same shape.
    #[test]
    fn registration_omits_empty_endpoints() {
        let json = serde_json::to_value(request(None)).unwrap();
        assert!(json.get("endpoints").is_none(), "{json}");
        assert_eq!(json["endpoints_legacy"], false);
    }
}
