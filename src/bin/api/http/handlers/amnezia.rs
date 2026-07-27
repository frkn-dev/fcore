use crate::sync::MemSync;
use base64::Engine;
use flate2::write::GzEncoder;
use flate2::Compression;
use fcore::{
    http::helpers as http, Connection, ConnectionApiOperations, ConnectionBaseOperations,
    ConnectionStorageApiOperations, InboundConnLink, NodeStatus, NodeStorageOperations,
    SubscriptionOperations, SubscriptionStorageOperations, Tag,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use warp::Reply;

// ============================================================================
// DTOs
// ============================================================================

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GatewayServicesRequest {
    #[serde(rename = "os_version")]
    pub os_version: Option<String>,
    #[serde(rename = "app_language")]
    pub app_language: Option<String>,
    #[serde(rename = "auth_data")]
    pub auth_data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct GatewayServicesResponse {
    #[serde(rename = "user_country_code", skip_serializing_if = "Option::is_none")]
    pub user_country_code: Option<String>,
    pub services: Vec<GatewayService>,
}

/// Labels shown on the client's service cards (from service config).
#[derive(Debug, Clone)]
pub struct GatewayLabels {
    pub price: String,
    pub speed: String,
}

#[derive(Debug, Serialize)]
pub struct GatewayService {
    #[serde(rename = "service_type")]
    pub service_type: String,
    #[serde(rename = "service_protocol")]
    pub service_protocol: String,
    #[serde(rename = "service_info")]
    pub service_info: GatewayServiceInfo,
    #[serde(rename = "service_description")]
    pub service_description: GatewayServiceDescription,
    #[serde(rename = "available_countries")]
    pub available_countries: Vec<GatewayCountry>,
    #[serde(rename = "connections")]
    pub connections: Vec<GatewayConnection>,
    #[serde(rename = "store_endpoint")]
    pub store_endpoint: String,
    #[serde(rename = "is_available")]
    pub is_available: bool,
    pub subscription: GatewaySubscriptionMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct GatewayServiceInfo {
    pub name: String,
    pub price: String,
    pub speed: String,
    pub timelimit: String,
    pub region: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GatewayServiceDescription {
    pub description: String,
    #[serde(rename = "card_description")]
    pub card_description: String,
    pub features: String,
}

#[derive(Debug, Serialize)]
pub struct GatewayCountry {
    #[serde(rename = "country_code")]
    pub country_code: String,
    #[serde(rename = "country_name")]
    pub country_name: String,
    #[serde(rename = "connection_uuid")]
    pub connection_uuid: Option<uuid::Uuid>,
    #[serde(rename = "connection_label")]
    pub connection_label: String,
}

#[derive(Debug, Serialize)]
pub struct GatewayConnection {
    #[serde(rename = "connection_uuid")]
    pub connection_uuid: uuid::Uuid,
    #[serde(rename = "country_code")]
    pub country_code: String,
    #[serde(rename = "country_name")]
    pub country_name: String,
    #[serde(rename = "connection_label")]
    pub connection_label: String,
}

#[derive(Debug, Serialize)]
pub struct GatewaySubscriptionMeta {
    #[serde(rename = "end_date")]
    pub end_date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GatewayAccountInfoRequest {
    #[serde(rename = "user_country_code")]
    pub user_country_code: String,
    #[serde(rename = "service_type")]
    pub service_type: String,
    #[serde(rename = "auth_data")]
    pub auth_data: serde_json::Value,
    #[serde(rename = "cli_version")]
    pub cli_version: Option<String>,
    #[serde(rename = "app_language")]
    pub app_language: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GatewayAccountInfoResponse {
    #[serde(rename = "supported_protocols")]
    pub supported_protocols: Vec<String>,
    #[serde(rename = "available_countries")]
    pub available_countries: Vec<GatewayCountry>,
    #[serde(rename = "active_device_count")]
    pub active_device_count: i64,
    #[serde(rename = "max_device_count")]
    pub max_device_count: i64,
    #[serde(rename = "subscription_end_date")]
    pub subscription_end_date: String,
    #[serde(rename = "subscription_description")]
    pub subscription_description: String,
    #[serde(rename = "issued_configs")]
    pub issued_configs: Vec<GatewayIssuedConfig>,
    #[serde(rename = "support_info")]
    pub support_info: GatewaySupportInfo,
}

#[derive(Debug, Serialize)]
pub struct GatewayIssuedConfig {
    #[serde(rename = "server_country_code")]
    pub server_country_code: String,
    #[serde(rename = "worker_last_updated")]
    pub worker_last_updated: String,
    #[serde(rename = "last_downloaded")]
    pub last_downloaded: String,
    #[serde(rename = "source_type")]
    pub source_type: String,
    #[serde(rename = "installation_uuid")]
    pub installation_uuid: String,
    #[serde(rename = "os_version")]
    pub os_version: String,
}

#[derive(Debug, Serialize)]
pub struct GatewaySupportInfo {
    pub email: String,
    #[serde(rename = "billing_email")]
    pub billing_email: String,
    pub website: String,
    #[serde(rename = "website_name")]
    pub website_name: String,
    pub telegram: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GatewayConfigRequest {
    #[serde(rename = "os_version")]
    pub os_version: String,
    #[serde(rename = "app_version")]
    pub app_version: String,
    #[serde(rename = "app_language")]
    pub app_language: String,
    #[serde(rename = "installation_uuid")]
    pub installation_uuid: String,
    #[serde(rename = "user_country_code")]
    pub user_country_code: String,
    #[serde(rename = "server_country_code")]
    pub server_country_code: Option<String>,
    #[serde(rename = "service_type")]
    pub service_type: String,
    #[serde(rename = "service_protocol")]
    pub service_protocol: String,
    #[serde(rename = "auth_data")]
    pub auth_data: serde_json::Value,
    #[serde(rename = "public_key")]
    pub public_key: Option<String>,
    #[serde(rename = "connection_id")]
    pub connection_id: Option<uuid::Uuid>,
}

#[derive(Debug, Serialize)]
pub struct GatewayConfigResponse {
    pub config: String, // base64-encoded Amnezia server config
    #[serde(rename = "supported_protocols")]
    pub supported_protocols: Vec<String>,
    #[serde(rename = "service_info")]
    pub service_info: serde_json::Value,
    #[serde(rename = "api_config")]
    pub api_config: serde_json::Value,
}

// ============================================================================
// Helpers
// ============================================================================

/// Extracts subscription_id from auth_data.
fn extract_subscription_id(auth_data: &serde_json::Value) -> Option<uuid::Uuid> {
    auth_data
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
}

/// Checks whether the tag matches the protocol from the client request.
fn proto_matches(tag: Tag, protocol: &str) -> bool {
    match protocol {
        "awg" => tag == Tag::AmneziaWg,
        "vless" => matches!(
            tag,
            Tag::VlessTcpReality
                | Tag::VlessGrpcReality
                | Tag::VlessXhttpReality
                | Tag::VlessXhttpCdn
        ),
        _ => false,
    }
}

/// Returns a human-readable inbound name for the given tag.
fn inbound_label(tag: Tag) -> &'static str {
    match tag {
        Tag::VlessTcpReality => "VLESS TCP Reality",
        Tag::VlessGrpcReality => "VLESS gRPC Reality",
        Tag::VlessXhttpReality => "VLESS XHTTP Reality",
        Tag::VlessXhttpCdn => "VLESS XHTTP CDN",
        Tag::AmneziaWg => "AmneziaWG",
        Tag::Wireguard => "WireGuard",
        Tag::Shadowsocks => "Shadowsocks",
        Tag::Hysteria2 => "Hysteria2",
        Tag::Mtproto => "MTProto",
        Tag::Vmess => "VMess",
    }
}

/// Returns the list of connections for the given protocol.
/// For each online node with the required inbound, finds a matching connection of the subscription.
fn connections_for_protocol<N, C>(
    nodes: &N,
    protocol: &str,
    conns: Option<&[(uuid::Uuid, C)]>,
) -> Vec<GatewayConnection>
where
    N: NodeStorageOperations,
    C: ConnectionApiOperations + ConnectionBaseOperations,
{
    let mut result = Vec::new();
    for (_, node) in nodes.iter_nodes() {
        if node.status != NodeStatus::Online {
            continue;
        }
        let code = node.country.to_uppercase();
        if let Some(cs) = conns {
            for (conn_id, conn) in cs {
                if conn.get_deleted() {
                    continue;
                }
                let tag = conn.get_proto().proto();
                if !proto_matches(tag, protocol) {
                    continue;
                }
                // A connection can only be matched with a node that has exactly the same inbound.
                if !node.inbounds.values().any(|i| i.tag == tag) {
                    continue;
                }
                if conn.get_env() != node.env {
                    continue;
                }
                let label = if node.label.is_empty() {
                    format!("{} · {}", code, inbound_label(tag))
                } else {
                    format!("{} · {}", node.label, inbound_label(tag))
                };
                result.push(GatewayConnection {
                    connection_uuid: *conn_id,
                    country_code: code.clone(),
                    country_name: code.clone(),
                    connection_label: label,
                });
            }
        }
    }
    if result.is_empty() {
        result = vec![GatewayConnection {
            connection_uuid: uuid::Uuid::nil(),
            country_code: String::new(),
            country_name: "All countries".to_string(),
            connection_label: "All countries".to_string(),
        }];
    }
    result
}

/// Returns a unique list of countries from the connections (for available_countries).
fn available_countries_from_connections(conns: &[GatewayConnection]) -> Vec<GatewayCountry> {
    let mut seen = std::collections::HashSet::new();
    let mut countries = Vec::new();
    for conn in conns {
        if seen.insert(conn.country_code.clone()) {
            countries.push(GatewayCountry {
                country_code: conn.country_code.clone(),
                country_name: conn.country_name.clone(),
                connection_uuid: Some(conn.connection_uuid),
                connection_label: conn.connection_label.clone(),
            });
        }
    }
    countries
}

/// Builds the Amnezia server config for AWG.
fn build_awg_server_config(
    ini_config: &str,
    client_priv_key: &str,
    client_pub_key: &str,
    hostname: &str,
    port: u16,
) -> serde_json::Value {
    let mut ini = HashMap::new();
    for line in ini_config.lines() {
        let line = line.trim();
        if line.starts_with('[') || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            ini.insert(k.trim(), v.trim());
        }
    }

    let client_ip = ini.get("Address").unwrap_or(&"10.8.1.2/32");
    let mtu = ini.get("MTU").unwrap_or(&"1420");
    let server_pub_key = ini.get("PublicKey").unwrap_or(&"");
    let psk_key = ini.get("PresharedKey").unwrap_or(&"");
    let persistent_keepalive = "25";

    let jc = ini.get("Jc").unwrap_or(&"4");
    let jmin = ini.get("Jmin").unwrap_or(&"40");
    let jmax = ini.get("Jmax").unwrap_or(&"70");
    let s1 = ini.get("S1").unwrap_or(&"0");
    let s2 = ini.get("S2").unwrap_or(&"0");
    let s3 = ini.get("S3").unwrap_or(&"0");
    let s4 = ini.get("S4").unwrap_or(&"0");
    let h1 = ini.get("H1").unwrap_or(&"1");
    let h2 = ini.get("H2").unwrap_or(&"2");
    let h3 = ini.get("H3").unwrap_or(&"3");
    let h4 = ini.get("H4").unwrap_or(&"4");
    let i1 = ini.get("I1").unwrap_or(&"0");

    let last_config = serde_json::json!({
        "client_priv_key": client_priv_key,
        "client_pub_key": client_pub_key,
        "client_ip": client_ip,
        "mtu": mtu,
        "server_pub_key": server_pub_key,
        "psk_key": psk_key,
        "port": port,
        "hostName": hostname,
        "persistent_keepalive": persistent_keepalive,
        "junkPacketCount": jc,
        "junkPacketMinSize": jmin,
        "junkPacketMaxSize": jmax,
        "initPacketJunkSize": s1,
        "responsePacketJunkSize": s2,
        "cookieReplyPacketJunkSize": s3,
        "transportPacketJunkSize": s4,
        "initPacketMagicHeader": h1,
        "responsePacketMagicHeader": h2,
        "underloadPacketMagicHeader": h3,
        "transportPacketMagicHeader": h4,
        "specialJunk1": i1,
    });

    serde_json::json!({
        "config_version": 2,
        "defaultContainer": "amnezia-awg",
        "description": "FRKN AWG",
        "dns1": "1.1.1.1",
        "dns2": "1.0.0.1",
        "hostName": hostname,
        "name": "FRKN",
        "containers": [
            {
                "container": "amnezia-awg",
                "amnezia-awg": {
                    "config": ini_config,
                    "isThirdPartyConfig": true,
                    "last_config": last_config.to_string()
                }
            }
        ]
    })
}

/// Builds the Amnezia server config for VLESS/XRay from the real inbound.
fn build_vless_server_config(
    inbound: &fcore::Inbound,
    conn_id: &uuid::Uuid,
    hostname: &str,
) -> Result<serde_json::Value, fcore::Error> {
    use fcore::Network;

    let stream = inbound
        .stream_settings
        .as_ref()
        .ok_or_else(|| fcore::Error::Custom("Missing stream settings".into()))?;

    if inbound.tag == Tag::VlessXhttpCdn {
        let xhttp = stream
            .xhttp_settings
            .as_ref()
            .ok_or_else(|| fcore::Error::Custom("Missing xhttp settings".into()))?;

        let cdn_host = stream
            .tls_settings
            .as_ref()
            .and_then(|t| t.server_name.clone())
            .unwrap_or_else(|| hostname.to_string());

        let mut xhttp_settings = serde_json::json!({ "path": xhttp.path });
        if let Some(mode) = &xhttp.mode {
            xhttp_settings["mode"] = mode.clone().into();
        }
        if let Some(extra) = &xhttp.extra {
            xhttp_settings["extra"] = extra.clone();
        }

        let outbound = serde_json::json!({
            "outbounds": [
                {
                    "protocol": "vless",
                    "settings": {
                        "vnext": [
                            {
                                "address": cdn_host,
                                "port": inbound.port,
                                "users": [
                                    {
                                        "id": conn_id,
                                        "encryption": "none"
                                    }
                                ]
                            }
                        ]
                    },
                    "streamSettings": {
                        "network": "xhttp",
                        "security": "tls",
                        "tlsSettings": {
                            "serverName": cdn_host
                        },
                        "xhttpSettings": xhttp_settings
                    }
                }
            ]
        });

        let last_config = serde_json::json!({
            "config": outbound.to_string(),
            "last_config": outbound.to_string(),
            "isThirdPartyConfig": true
        });

        return Ok(serde_json::json!({
            "containers": [
                {
                    "container": "amnezia-xray",
                    "position": 2,
                    "config": outbound.to_string(),
                    "last_config": outbound.to_string(),
                    "isThirdPartyConfig": true
                }
            ],
            "defaultContainer": "amnezia-xray",
            "dns1": "1.1.1.1",
            "dns2": "1.0.0.1",
            "hostName": cdn_host,
            "description": "FRKN VLESS",
            "name": "FRKN",
            "config_version": 2,
            "last_config": last_config
        }));
    }

    let reality = stream
        .reality_settings
        .as_ref()
        .ok_or_else(|| fcore::Error::Custom("Missing reality settings".into()))?;

    let pbk = &reality.public_key;
    let sid = reality
        .short_ids
        .first()
        .ok_or_else(|| fcore::Error::Custom("Missing shortId".into()))?;
    let sni = reality
        .server_names
        .first()
        .ok_or_else(|| fcore::Error::Custom("Missing serverNames".into()))?;

    let outbound = match stream.network {
        Network::Tcp => serde_json::json!({
            "outbounds": [
                {
                    "protocol": "vless",
                    "settings": {
                        "vnext": [
                            {
                                "address": hostname,
                                "port": inbound.port,
                                "users": [
                                    {
                                        "id": conn_id,
                                        "encryption": "none",
                                        "flow": "xtls-rprx-vision"
                                    }
                                ]
                            }
                        ]
                    },
                    "streamSettings": {
                        "network": "tcp",
                        "security": "reality",
                        "realitySettings": {
                            "show": false,
                            "fingerprint": "chrome",
                            "serverName": sni,
                            "publicKey": pbk,
                            "shortId": sid,
                            "spiderX": ""
                        }
                    }
                }
            ]
        }),
        Network::Grpc => {
            let grpc = stream
                .grpc_settings
                .as_ref()
                .ok_or_else(|| fcore::Error::Custom("Missing grpc settings".into()))?;
            serde_json::json!({
                "outbounds": [
                    {
                        "protocol": "vless",
                        "settings": {
                            "vnext": [
                                {
                                    "address": hostname,
                                    "port": inbound.port,
                                    "users": [
                                        {
                                            "id": conn_id,
                                            "encryption": "none"
                                        }
                                    ]
                                }
                            ]
                        },
                        "streamSettings": {
                            "network": "grpc",
                            "security": "reality",
                            "grpcSettings": {
                                "serviceName": grpc.service_name
                            },
                            "realitySettings": {
                                "show": false,
                                "fingerprint": "chrome",
                                "serverName": sni,
                                "publicKey": pbk,
                                "shortId": sid,
                                "spiderX": ""
                            }
                        }
                    }
                ]
            })
        }
        Network::Xhttp => {
            let xhttp = stream
                .xhttp_settings
                .as_ref()
                .ok_or_else(|| fcore::Error::Custom("Missing xhttp settings".into()))?;
            serde_json::json!({
                "outbounds": [
                    {
                        "protocol": "vless",
                        "settings": {
                            "vnext": [
                                {
                                    "address": hostname,
                                    "port": inbound.port,
                                    "users": [
                                        {
                                            "id": conn_id,
                                            "encryption": "none"
                                        }
                                    ]
                                }
                            ]
                        },
                        "streamSettings": {
                            "network": "xhttp",
                            "security": "reality",
                            "xhttpSettings": {
                                "path": xhttp.path
                            },
                            "realitySettings": {
                                "show": false,
                                "fingerprint": "chrome",
                                "serverName": sni,
                                "publicKey": pbk,
                                "shortId": sid,
                                "spiderX": ""
                            }
                        }
                    }
                ]
            })
        }
        _ => return Err(fcore::Error::Custom("Unsupported vless network".into())),
    };

    let last_config = serde_json::json!({
        "config": outbound.to_string(),
        "last_config": outbound.to_string(),
        "isThirdPartyConfig": true
    });

    Ok(serde_json::json!({
        "containers": [
            {
                "container": "amnezia-xray",
                "xray": last_config
            }
        ],
        "defaultContainer": "amnezia-xray",
        "dns1": "1.1.1.1",
        "dns2": "1.0.0.1",
        "hostName": hostname,
        "description": "FRKN VLESS",
        "name": "FRKN",
        "config_version": 2
    }))
}

// ============================================================================
// Handlers
// ============================================================================

pub async fn gateway_services_handler<N, C, S>(
    req: GatewayServicesRequest,
    memory: MemSync<N, C, S>,
    labels: GatewayLabels,
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq,
    Connection: From<C>,
{
    let mem = memory.memory.read().await;

    // If subscription_id is provided, use the real subscription end_date and its connections.
    let sub_id = req.auth_data.as_ref().and_then(extract_subscription_id);
    let end_date = sub_id
        .and_then(|sub_id| mem.subscriptions.find_by_id(&sub_id))
        .and_then(|sub| sub.expires_at().map(|d| d.to_rfc3339()));
    let conns = sub_id.and_then(|sub_id| mem.connections.get_by_subscription_id(&sub_id));
    let conns_slice = conns.as_deref();

    let vless_connections = connections_for_protocol(&mem.nodes, "vless", conns_slice);
    let awg_connections = connections_for_protocol(&mem.nodes, "awg", conns_slice);

    // One merged service: the client must not offer a protocol choice at purchase.
    let mut connections = vless_connections;
    connections.extend(awg_connections);
    let countries = available_countries_from_connections(&connections);

    let info = GatewayServiceInfo {
        name: "FRKN Premium".to_string(),
        price: labels.price.clone(),
        speed: labels.speed.clone(),
        timelimit: "0".to_string(),
        region: "World".to_string(),
    };
    let description = GatewayServiceDescription {
        description: "Privacy is our Religion".to_string(),
        card_description: "FRKN Premium — обход блокировок".to_string(),
        features: "No logs, unlimited traffic, VLESS and AmneziaWG protocols".to_string(),
    };

    let services = vec![GatewayService {
        service_type: "amnezia-premium".to_string(),
        service_protocol: "vless".to_string(),
        service_info: info,
        service_description: description,
        available_countries: countries,
        connections,
        store_endpoint: "https://frkn.org".to_string(),
        is_available: true,
        subscription: GatewaySubscriptionMeta { end_date },
    }];

    Ok(warp::reply::json(&GatewayServicesResponse {
        // No geoip on the gateway yet — better to omit the field than lie.
        user_country_code: None,
        services,
    })
    .into_response())
}

pub async fn gateway_account_info_handler<N, C, S>(
    req: GatewayAccountInfoRequest,
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq,
    Connection: From<C>,
{
    let sub_id = match extract_subscription_id(&req.auth_data) {
        Some(id) => id,
        None => {
            return Ok(warp::reply::with_status(
                "Missing subscription id in auth_data",
                warp::http::StatusCode::BAD_REQUEST,
            )
            .into_response())
        }
    };

    let mem = memory.memory.read().await;

    let sub = match mem.subscriptions.find_by_id(&sub_id) {
        Some(s) => s,
        None => return Ok(http::not_found("Subscription not found").into_response()),
    };

    if !sub.is_active() {
        return Ok(http::not_found("Subscription expired").into_response());
    }

    let conns = mem.connections.get_by_subscription_id(&sub_id);
    let active_devices = conns.as_ref().map(|c| c.len() as i64).unwrap_or(0);

    // Build issued_configs from real connections.
    let issued_configs: Vec<GatewayIssuedConfig> = conns
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, conn)| !conn.get_deleted())
        .map(|(conn_id, conn)| {
            let node_country = mem
                .nodes
                .get_by_env(&conn.get_env())
                .and_then(|nodes| nodes.first().map(|n| n.country.clone()))
                .unwrap_or_else(|| conn.get_env().to_string());

            GatewayIssuedConfig {
                server_country_code: node_country.to_uppercase(),
                worker_last_updated: conn.get_modified_at().to_rfc3339(),
                last_downloaded: conn.get_modified_at().to_rfc3339(),
                source_type: "country_config".to_string(),
                installation_uuid: conn_id.to_string(),
                os_version: String::new(),
            }
        })
        .collect();

    let subscription_end_date = sub
        .expires_at()
        .map(|d| d.to_rfc3339())
        .unwrap_or_else(|| "2099-01-01T00:00:00Z".to_string());

    Ok(warp::reply::json(&GatewayAccountInfoResponse {
        supported_protocols: vec!["vless".to_string(), "awg".to_string()],
        available_countries: vec![
            GatewayCountry {
                country_code: String::new(),
                country_name: "All countries".to_string(),
                connection_uuid: None,
                connection_label: "All countries".to_string(),
            },
        ],
        active_device_count: active_devices,
        max_device_count: 5,
        subscription_end_date,
        subscription_description: "FRKN Premium subscription".to_string(),
        issued_configs,
        support_info: GatewaySupportInfo {
            email: "support@frkn.org".to_string(),
            billing_email: "billing@frkn.org".to_string(),
            website: "https://frkn.org".to_string(),
            website_name: "FRKN".to_string(),
            telegram: "https://t.me/frkn_org".to_string(),
        },
    }).into_response())
}

fn gzip_json(value: &serde_json::Value) -> Result<Vec<u8>, fcore::Error> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(value.to_string().as_bytes())
        .map_err(|e| fcore::Error::Custom(format!("gzip encode failed: {}", e)))?;
    encoder
        .finish()
        .map_err(|e| fcore::Error::Custom(format!("gzip finish failed: {}", e)))
}

/// Parameters for building a gateway VPN config, shared by
/// `gateway_config_handler` and `gateway_subscriptions_handler`.
pub struct GatewayConfigParams<'a> {
    pub service_protocol: &'a str,
    pub service_type: &'a str,
    pub user_country_code: &'a str,
    pub server_country_code: Option<&'a str>,
    pub connection_id: Option<uuid::Uuid>,
    pub public_key: Option<&'a str>,
}

/// Builds the gateway config for an active subscription: picks a matching
/// (connection, online node) pair and renders the Amnezia server config.
///
/// Returns `Err(response)` with a ready error response (4xx) on failure.
pub async fn build_gateway_config_response<N, C, S>(
    memory: &MemSync<N, C, S>,
    sub_id: &uuid::Uuid,
    params: &GatewayConfigParams<'_>,
) -> Result<GatewayConfigResponse, warp::reply::Response>
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq,
    Connection: From<C>,
{
    let mem = memory.memory.read().await;

    let sub = match mem.subscriptions.find_by_id(sub_id) {
        Some(s) => s,
        None => return Err(http::not_found("Subscription not found").into_response()),
    };

    if !sub.is_active() {
        return Err(http::not_found("Subscription expired").into_response());
    }

    let conns = match mem.connections.get_by_subscription_id(sub_id) {
        Some(c) => c,
        None => return Err(http::not_found("No connections").into_response()),
    };

    let target_country = params
        .server_country_code
        .unwrap_or(params.user_country_code);

    let mut found_conn = None;
    let mut found_node = None;
    let mut found_conn_id = None;

    for (conn_id, conn) in conns {
        if conn.get_deleted() {
            continue;
        }
        let conn_tag = conn.get_proto().proto();
        if !proto_matches(conn_tag, params.service_protocol) {
            continue;
        }
        if let Some(requested_id) = params.connection_id {
            if conn_id != requested_id {
                continue;
            }
        }

        if let Some(nodes) = mem.nodes.get_by_env(&conn.get_env()) {
            for node in nodes {
                if node.status != NodeStatus::Online {
                    continue;
                }

                // The node must have exactly the same inbound as the connection.
                let has_inbound = node.inbounds.values().any(|i| i.tag == conn_tag);
                if !has_inbound {
                    continue;
                }

                if !target_country.is_empty()
                    && node.country.to_lowercase() != target_country.to_lowercase()
                {
                    continue;
                }

                let c: Connection = conn.clone().into();
                found_conn = Some(c);
                found_node = Some(node.clone());
                found_conn_id = Some(conn_id);
                break;
            }
        }
        if found_conn.is_some() {
            break;
        }
    }

    let (conn, node, conn_id) = match (found_conn, found_node, found_conn_id) {
        (Some(c), Some(n), Some(id)) => (c, n, id),
        _ => {
            return Err(http::not_found(
                "No suitable connection/node found",
            )
            .into_response())
        }
    };

    let server_config_json = match params.service_protocol {
        "awg" => {
            let inbound = node.inbounds.get(&Tag::AmneziaWg).unwrap();
            let host = node.connection_host();
            let link = inbound
                .create_link(&conn_id, &conn, &node.hostname, &host, &node.label)
                .map_err(|_| {
                    http::internal_error("Failed to build AWG link").into_response()
                })?;

            let awg_param = conn.get_amneziawg().ok_or_else(|| {
                http::internal_error("Connection has no AmneziaWG params").into_response()
            })?;
            let client_priv_key = awg_param.keys.privkey.clone();
            let client_pub_key = awg_param.keys.pubkey().map_err(|_| {
                http::internal_error("Failed to derive AmneziaWG public key").into_response()
            })?;

            let port = inbound.port;

            build_awg_server_config(&link, &client_priv_key, &client_pub_key, &host, port)
        }
        "vless" => {
            let inbound = node
                .inbounds
                .values()
                .find(|i| proto_matches(i.tag, "vless"))
                .ok_or_else(|| {
                    http::internal_error("Node has no VLESS inbound").into_response()
                })?;

            let xray_uuid = params.public_key.unwrap_or("");
            let conn_id = uuid::Uuid::parse_str(xray_uuid).unwrap_or(conn_id);
            let host = node.connection_host();
            build_vless_server_config(inbound, &conn_id, &host)
                .map_err(|_| http::internal_error("Failed to build VLESS config").into_response())?
        }
        _ => {
            return Err(http::bad_request("Unsupported service_protocol").into_response())
        }
    };

    let is_awg = params.service_protocol == "awg";
    let config_bytes = if is_awg {
        gzip_json(&server_config_json)
            .map_err(|_| http::internal_error("Failed to gzip config").into_response())?
    } else {
        server_config_json.to_string().into_bytes()
    };
    let config_b64 = if is_awg {
        base64::engine::general_purpose::STANDARD.encode(&config_bytes)
    } else {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&config_bytes)
    };

    Ok(GatewayConfigResponse {
        config: config_b64,
        supported_protocols: vec!["vless".to_string(), "awg".to_string()],
        service_info: serde_json::json!({
            "name": node.label,
            "type": params.service_type
        }),
        api_config: serde_json::json!({
            "service_type": params.service_type,
            "service_protocol": params.service_protocol,
            "user_country_code": params.user_country_code,
            "server_country_code": params.server_country_code
        }),
    })
}

pub async fn gateway_config_handler<N, C, S>(
    req: GatewayConfigRequest,
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
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq,
    Connection: From<C>,
{
    let sub_id = match extract_subscription_id(&req.auth_data) {
        Some(id) => id,
        None => {
            return Ok(warp::reply::with_status(
                "Missing subscription id in auth_data",
                warp::http::StatusCode::BAD_REQUEST,
            )
            .into_response())
        }
    };

    let params = GatewayConfigParams {
        service_protocol: &req.service_protocol,
        service_type: &req.service_type,
        user_country_code: &req.user_country_code,
        server_country_code: req.server_country_code.as_deref(),
        connection_id: req.connection_id,
        public_key: req.public_key.as_deref(),
    };

    match build_gateway_config_response(&memory, &sub_id, &params).await {
        Ok(response) => Ok(warp::reply::json(&response).into_response()),
        Err(response) => Ok(response),
    }
}
