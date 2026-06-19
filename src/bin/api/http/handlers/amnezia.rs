use crate::sync::MemSync;
use base64::Engine;
use fcore::{
    http::helpers as http,
    Connection, ConnectionApiOperations, ConnectionBaseOperations, ConnectionStorageApiOperations,
    InboundConnLink, NodeStatus, NodeStorageOperations, SubscriptionOperations,
    SubscriptionStorageOperations, Tag,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
}

#[derive(Debug, Serialize)]
pub struct GatewayServicesResponse {
    #[serde(rename = "user_country_code")]
    pub user_country_code: String,
    pub services: Vec<GatewayService>,
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
    #[serde(rename = "store_endpoint")]
    pub store_endpoint: String,
    #[serde(rename = "is_available")]
    pub is_available: bool,
    pub subscription: GatewaySubscriptionMeta,
}

#[derive(Debug, Serialize)]
pub struct GatewayServiceInfo {
    pub name: String,
    pub price: String,
    pub speed: String,
    pub timelimit: String,
    pub region: String,
}

#[derive(Debug, Serialize)]
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
// Хелперы
// ============================================================================

/// Извлекает subscription_id из auth_data.
fn extract_subscription_id(auth_data: &serde_json::Value) -> Option<uuid::Uuid> {
    auth_data
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
}

/// Проверяет, соответствует ли тег протоколу из запроса клиента.
fn proto_matches(tag: Tag, protocol: &str) -> bool {
    match protocol {
        "awg" => tag == Tag::AmneziaWg,
        "vless" => matches!(
            tag,
            Tag::VlessTcpReality | Tag::VlessGrpcReality | Tag::VlessXhttpReality
        ),
        _ => false,
    }
}

/// Строит Amnezia server config для AWG.
fn build_awg_server_config(
    ini_config: &str,
    client_pub_key: &str,
    hostname: &str,
    port: u16,
) -> serde_json::Value {
    let mut ini = HashMap::new();
    for line in ini_config.lines() {
        let line = line.trim();
        if line.starts_with('[') || line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            ini.insert(k.trim(), v.trim());
        }
    }

    let last_config = serde_json::json!({
        "client_ip": ini.get("Address").unwrap_or(&"10.8.1.2/32"),
        "client_private_key": "$WIREGUARD_CLIENT_PRIVATE_KEY",
        "client_pub_key": client_pub_key,
        "hostName": hostname,
        "port": port,
        "server_pub_key": ini.get("PublicKey").unwrap_or(&""),
        "psk_key": ini.get("PresharedKey").unwrap_or(&""),
        "mtu": ini.get("MTU").unwrap_or(&"1420"),
        "Jc": ini.get("Jc").unwrap_or(&"4"),
        "Jmin": ini.get("Jmin").unwrap_or(&"40"),
        "Jmax": ini.get("Jmax").unwrap_or(&"70"),
        "S1": ini.get("S1").unwrap_or(&"0"),
        "S2": ini.get("S2").unwrap_or(&"0"),
        "S3": ini.get("S3").unwrap_or(&"0"),
        "S4": ini.get("S4").unwrap_or(&"0"),
        "H1": ini.get("H1").unwrap_or(&"1"),
        "H2": ini.get("H2").unwrap_or(&"2"),
        "H3": ini.get("H3").unwrap_or(&"3"),
        "H4": ini.get("H4").unwrap_or(&"4"),
        "I1": ini.get("I1").unwrap_or(&"0"),
        "I2": ini.get("I2").unwrap_or(&"0"),
        "I3": ini.get("I3").unwrap_or(&"0"),
        "I4": ini.get("I4").unwrap_or(&"0"),
        "I5": ini.get("I5").unwrap_or(&"0"),
    });

    serde_json::json!({
        "containers": [
            {
                "container": "amnezia-awg",
                "awg": {
                    "last_config": last_config.to_string(),
                    "port": port,
                    "Jc": ini.get("Jc").unwrap_or(&"4"),
                    "Jmin": ini.get("Jmin").unwrap_or(&"40"),
                    "Jmax": ini.get("Jmax").unwrap_or(&"70"),
                    "S1": ini.get("S1").unwrap_or(&"0"),
                    "S2": ini.get("S2").unwrap_or(&"0"),
                    "S3": ini.get("S3").unwrap_or(&"0"),
                    "S4": ini.get("S4").unwrap_or(&"0"),
                    "H1": ini.get("H1").unwrap_or(&"1"),
                    "H2": ini.get("H2").unwrap_or(&"2"),
                    "H3": ini.get("H3").unwrap_or(&"3"),
                    "H4": ini.get("H4").unwrap_or(&"4"),
                    "I1": ini.get("I1").unwrap_or(&"0"),
                    "I2": ini.get("I2").unwrap_or(&"0"),
                    "I3": ini.get("I3").unwrap_or(&"0"),
                    "I4": ini.get("I4").unwrap_or(&"0"),
                    "I5": ini.get("I5").unwrap_or(&"0"),
                }
            }
        ],
        "defaultContainer": "amnezia-awg",
        "dns1": "1.1.1.1",
        "dns2": "1.0.0.1",
        "hostName": hostname,
        "description": "FRKN AWG",
        "name": "FRKN",
        "config_version": 2
    })
}

/// Строит Amnezia server config для VLESS/XRay из реального inbound.
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
            let grpc = stream.grpc_settings.as_ref().ok_or_else(|| {
                fcore::Error::Custom("Missing grpc settings".into())
            })?;
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
            let xhttp = stream.xhttp_settings.as_ref().ok_or_else(|| {
                fcore::Error::Custom("Missing xhttp settings".into())
            })?;
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
// Обработчики
// ============================================================================

pub async fn gateway_services_handler<N, C, S>(
    _req: GatewayServicesRequest,
    memory: MemSync<N, C, S>,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection>
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

    // Собираем уникальные страны из реальных нод
    let mut seen = std::collections::HashSet::new();
    let mut available_countries = Vec::new();
    for (_, node) in mem.nodes.iter_nodes() {
        let code = node.country.to_uppercase();
        if seen.insert(code.clone()) {
            available_countries.push(GatewayCountry {
                country_code: code.clone(),
                country_name: code,
            });
        }
    }
    if available_countries.is_empty() {
        available_countries = vec![
            GatewayCountry {
                country_code: "NL".to_string(),
                country_name: "Netherlands".to_string(),
            },
            GatewayCountry {
                country_code: "DE".to_string(),
                country_name: "Germany".to_string(),
            },
        ];
    }

    let user_country_code = "RU".to_string();

    let service = GatewayService {
        service_type: "amnezia-free".to_string(),
        service_protocol: "vless".to_string(),
        service_info: GatewayServiceInfo {
            name: "Free".to_string(),
            price: "free".to_string(),
            speed: "100".to_string(),
            timelimit: "0".to_string(),
            region: "World".to_string(),
        },
        service_description: GatewayServiceDescription {
            description: "Free VPN by FRKN".to_string(),
            card_description: "Free VPN with unlimited traffic".to_string(),
            features: "No logs, unlimited traffic".to_string(),
        },
        available_countries,
        store_endpoint: "https://frkn.org".to_string(),
        is_available: true,
        subscription: GatewaySubscriptionMeta { end_date: None },
    };

    Ok(Box::new(warp::reply::json(&GatewayServicesResponse {
        user_country_code,
        services: vec![service],
    })))
}

pub async fn gateway_account_info_handler<N, C, S>(
    req: GatewayAccountInfoRequest,
    memory: MemSync<N, C, S>,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection>
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
            return Ok(Box::new(warp::reply::with_status(
                "Missing subscription id in auth_data",
                warp::http::StatusCode::BAD_REQUEST,
            )))
        }
    };

    let mem = memory.memory.read().await;

    let sub = match mem.subscriptions.find_by_id(&sub_id) {
        Some(s) => s,
        None => return Ok(Box::new(http::not_found("Subscription not found"))),
    };

    if !sub.is_active() {
        return Ok(Box::new(http::not_found("Subscription expired")));
    }

    let conns = mem.connections.get_by_subscription_id(&sub_id);
    let active_devices = conns.as_ref().map(|c| c.len() as i64).unwrap_or(0);

    // Собираем issued_configs из реальных connections
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

    Ok(Box::new(warp::reply::json(&GatewayAccountInfoResponse {
        supported_protocols: vec!["vless".to_string(), "awg".to_string()],
        available_countries: vec![
            GatewayCountry {
                country_code: "NL".to_string(),
                country_name: "Netherlands".to_string(),
            },
            GatewayCountry {
                country_code: "DE".to_string(),
                country_name: "Germany".to_string(),
            },
        ],
        active_device_count: active_devices,
        max_device_count: 5,
        subscription_end_date,
        subscription_description: "FRKN Free subscription".to_string(),
        issued_configs,
        support_info: GatewaySupportInfo {
            email: "support@frkn.org".to_string(),
            billing_email: "billing@frkn.org".to_string(),
            website: "https://frkn.org".to_string(),
            website_name: "FRKN".to_string(),
            telegram: "https://t.me/frkn_org".to_string(),
        },
    })))
}

pub async fn gateway_config_handler<N, C, S>(
    req: GatewayConfigRequest,
    memory: MemSync<N, C, S>,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection>
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
            return Ok(Box::new(warp::reply::with_status(
                "Missing subscription id in auth_data",
                warp::http::StatusCode::BAD_REQUEST,
            )))
        }
    };

    let mem = memory.memory.read().await;

    let sub = match mem.subscriptions.find_by_id(&sub_id) {
        Some(s) => s,
        None => return Ok(Box::new(http::not_found("Subscription not found"))),
    };

    if !sub.is_active() {
        return Ok(Box::new(http::not_found("Subscription expired")));
    }

    let conns = match mem.connections.get_by_subscription_id(&sub_id) {
        Some(c) => c,
        None => return Ok(Box::new(http::not_found("No connections"))),
    };

    let target_country = req
        .server_country_code
        .as_deref()
        .unwrap_or(&req.user_country_code);

    let mut found_conn = None;
    let mut found_node = None;
    let mut found_conn_id = None;

    for (conn_id, conn) in conns {
        if conn.get_deleted() {
            continue;
        }
        if !proto_matches(conn.get_proto().proto(), &req.service_protocol) {
            continue;
        }

        if let Some(nodes) = mem.nodes.get_by_env(&conn.get_env()) {
            for node in nodes {
                if node.status != NodeStatus::Online {
                    continue;
                }

                let has_inbound = node
                    .inbounds
                    .values()
                    .any(|i| proto_matches(i.tag, &req.service_protocol));
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
            return Ok(Box::new(http::not_found(
                "No suitable connection/node found",
            )))
        }
    };

    let server_config_json = match req.service_protocol.as_str() {
        "awg" => {
            let inbound = node.inbounds.get(&Tag::AmneziaWg).unwrap();
            let link = inbound
                .create_link(&conn_id, &conn, &node.hostname, &node.address, &node.label)
                .map_err(|_| warp::reject::not_found())?;

            let port = inbound.port;

            build_awg_server_config(
                &link,
                req.public_key.as_deref().unwrap_or(""),
                &node.hostname,
                port,
            )
        }
        "vless" => {
            let inbound = node
                .inbounds
                .values()
                .find(|i| proto_matches(i.tag, "vless"))
                .ok_or_else(|| warp::reject::not_found())?;

            let xray_uuid = req.public_key.as_deref().unwrap_or("");
            let conn_id = uuid::Uuid::parse_str(xray_uuid).unwrap_or(conn_id);
            build_vless_server_config(inbound, &conn_id, &node.hostname)
                .map_err(|_| warp::reject::not_found())?
        }
        _ => unreachable!(),
    };

    let config_str = server_config_json.to_string();
    let config_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(config_str);

    Ok(Box::new(warp::reply::json(&GatewayConfigResponse {
        config: config_b64,
        supported_protocols: vec!["vless".to_string(), "awg".to_string()],
        service_info: serde_json::json!({
            "name": node.label,
            "type": req.service_type
        }),
        api_config: serde_json::json!({
            "service_type": req.service_type,
            "service_protocol": req.service_protocol,
            "user_country_code": req.user_country_code,
            "server_country_code": req.server_country_code
        }),
    })))
}
