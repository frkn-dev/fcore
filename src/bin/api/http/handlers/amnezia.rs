use super::{resolve_serving_access, serving_access, ServingAccess};
use crate::sync::MemSync;
use base64::Engine;
use fcore::{
    http::helpers as http, Connection, ConnectionApiOperations, ConnectionBaseOperations,
    ConnectionStorageApiOperations, InboundConnLink, MetricStorage, NodeStatus,
    NodeStorageOperations, SubscriptionOperations, SubscriptionStorageOperations, Tag,
    DEFAULT_NODE_WEIGHT,
};
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use warp::Reply;

use super::super::request::normalized_pool;
use super::connection::{main_feed_connection_visible, pinned_to};
use super::share;

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
    #[serde(default)]
    pub pool_off: bool,
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
    /// Pool/group name of the node (config.toml `[node] pool`). Absent for
    /// old nodes so the old response shape is preserved.
    #[serde(rename = "pool", skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Per-language translations of `pool`; absent when there is none.
    #[serde(rename = "pool_translation", skip_serializing_if = "Option::is_none")]
    pub pool_translation: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GatewayConnection {
    #[serde(rename = "connection_uuid")]
    pub connection_uuid: uuid::Uuid,
    #[serde(rename = "node_id")]
    pub node_id: uuid::Uuid,
    #[serde(rename = "country_code")]
    pub country_code: String,
    #[serde(rename = "country_name")]
    pub country_name: String,
    #[serde(rename = "service_protocol")]
    pub service_protocol: String,
    #[serde(rename = "env")]
    pub env: String,
    #[serde(rename = "connection_label")]
    pub connection_label: String,
    /// All entry IPs of the node as plain IP strings (first = primary).
    /// Absent for single-address nodes so old clients see the old shape.
    #[serde(rename = "node_ips", skip_serializing_if = "Option::is_none")]
    pub node_ips: Option<Vec<String>>,
    /// Port of each pool address: `{"93.89.220.48": 443, "77.221.152.88": 8443}`.
    ///
    /// Key — an address from `node_ips`, value — the port this address listens
    /// on for **its** protocol. Addresses of one node share one interface and
    /// therefore one port; ports may differ between nodes of a pool.
    ///
    /// `node_ips` stays the single list of addresses — the map only adds ports
    /// to it. The client (`servers_model.cpp::portForNodeIp`) reads
    /// `node_ports[ip]` and falls back to the protocol port when the key is
    /// missing, so an old api build (no field) and a node with a regular port
    /// behave the same.
    #[serde(rename = "node_ports", skip_serializing_if = "Option::is_none")]
    pub node_ports: Option<BTreeMap<String, u16>>,
    /// Pool/group name of the node (config.toml `[node] pool`). Clients
    /// group identically-named nodes and may localize the title via
    /// `pool_translation`. Absent for old nodes.
    #[serde(rename = "pool", skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Per-language translations of `pool`; absent when there is none.
    #[serde(rename = "pool_translation", skip_serializing_if = "Option::is_none")]
    pub pool_translation: Option<BTreeMap<String, String>>,
    /// Legacy field for old clients: they require a numeric `weight` and drop
    /// the whole server without it. Value — the highest address weight
    /// (100 by default).
    #[serde(rename = "weight")]
    pub weight: i32,
    /// Weight of every entry address: key — an address from `node_ips`/
    /// `node_ports`, value — its weight (100 when not set). Different addresses
    /// may share the same weight.
    #[serde(rename = "node_weights", skip_serializing_if = "Option::is_none")]
    pub node_weights: Option<BTreeMap<String, i32>>,
    #[serde(rename = "config_rev", skip_serializing_if = "Option::is_none")]
    pub config_rev: Option<String>,
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
    pub user_country_code: Option<String>,
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
    pub user_country_code: Option<String>,
    #[serde(rename = "server_country_code")]
    pub server_country_code: Option<String>,
    // Optional: meaningless under share_token auth (the token's child
    // connection determines everything). The owner branch below still
    // requires both fields with an explicit 400.
    #[serde(rename = "service_type")]
    pub service_type: Option<String>,
    #[serde(rename = "service_protocol")]
    pub service_protocol: Option<String>,
    #[serde(rename = "auth_data")]
    pub auth_data: serde_json::Value,
    #[serde(rename = "public_key")]
    pub public_key: Option<String>,
    #[serde(rename = "connection_id")]
    pub connection_id: Option<uuid::Uuid>,
    #[serde(rename = "node_id")]
    pub node_id: Option<uuid::Uuid>,
    // --- connect-event (frkn-docs/api-connect-event.md) ---
    // Всё опционально: старый клиент этих полей не шлёт, новый клиент со
    // старым api просто игнорируется serde (deny_unknown_fields не включён).
    /// Признак «это событие подключения, а не обычный refresh конфига».
    #[serde(rename = "is_connect_event", default)]
    pub is_connect_event: bool,
    /// IP, который клиент реально набрал (у пула адресов много).
    #[serde(rename = "endpoint", default)]
    pub endpoint: Option<String>,
    /// Порт этого адреса: у пула у каждого свой.
    #[serde(rename = "endpoint_port", default)]
    pub endpoint_port: Option<i64>,
    /// installation uuid — тот же, что в отчётах `v1/report/block`.
    #[serde(rename = "support_tag", default)]
    pub support_tag: Option<String>,
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
    // Display fields set only under share_token auth: the recipient has no
    // /v1/services access to learn them from.
    #[serde(rename = "share_label", skip_serializing_if = "Option::is_none")]
    pub share_label: Option<String>,
    #[serde(rename = "country_code", skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
    #[serde(rename = "country_name", skip_serializing_if = "Option::is_none")]
    pub country_name: Option<String>,
    #[serde(rename = "service_protocol", skip_serializing_if = "Option::is_none")]
    pub service_protocol: Option<String>,
    #[serde(rename = "service_type", skip_serializing_if = "Option::is_none")]
    pub service_type: Option<String>,
    /// All entry IPs of the chosen node as plain IP strings (first =
    /// primary); the client picks/rotates addresses itself. Absent for
    /// single-address nodes so old clients see the old shape.
    #[serde(rename = "node_ips", skip_serializing_if = "Option::is_none")]
    pub node_ips: Option<Vec<String>>,
    /// Порт конкретного адреса — та же карта, что в `/v1/services`, чтобы
    /// клиент не собирал её из двух ответов.
    #[serde(rename = "node_ports", skip_serializing_if = "Option::is_none")]
    pub node_ports: Option<BTreeMap<String, u16>>,
    #[serde(rename = "config_rev", skip_serializing_if = "Option::is_none")]
    pub config_rev: Option<String>,
}

// ============================================================================
// Helpers
// ============================================================================

/// Extracts subscription_id from auth_data.
pub(crate) fn extract_subscription_id(auth_data: &serde_json::Value) -> Option<uuid::Uuid> {
    auth_data
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
}

/// Checks whether the tag matches the protocol from the client request.
/// "awg-mobile" is a distinct protocol for the client; "awg" matches only
/// the classic tag so the two stay distinguishable in filters.
fn proto_matches(tag: Tag, protocol: &str) -> bool {
    match protocol {
        "awg" => tag == Tag::AmneziaWg,
        "awg-mobile" => tag == Tag::AmneziaWgMobile,
        "wireguard" => tag == Tag::Wireguard,
        "hysteria2" => tag == Tag::Hysteria2,
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

/// Protocol label reported back to the client (api_config.service_protocol).
fn proto_label(tag: Tag) -> &'static str {
    match tag {
        Tag::AmneziaWg => "awg",
        Tag::AmneziaWgMobile => "awg-mobile",
        Tag::Wireguard => "wireguard",
        Tag::Hysteria2 => "hysteria2",
        _ => "vless",
    }
}

/// Returns a human-readable inbound name for the given tag.
fn inbound_label(tag: Tag) -> &'static str {
    match tag {
        Tag::VlessTcpReality => "VLESS TCP Reality",
        Tag::VlessGrpcReality => "VLESS gRPC Reality",
        Tag::VlessXhttpReality => "VLESS XHTTP Reality",
        Tag::VlessXhttpCdn => "VLESS XHTTP CDN",
        Tag::AmneziaWg | Tag::AmneziaWgMobile => "AmneziaWG",
        Tag::Wireguard => "WireGuard",
        Tag::Shadowsocks => "Shadowsocks",
        Tag::Hysteria2 => "Hysteria2",
        Tag::Mtproto => "MTProto",
        Tag::Vmess => "VMess",
    }
}

fn label_is_host(label: &str, hostname: &str, address: &str) -> bool {
    let label = label.trim();
    label.is_empty() || label.eq_ignore_ascii_case(hostname.trim()) || label == address
}

/// Title of one (device, node) entry in the app's server list.
///
/// A named device keeps its name as the prefix and the node label in brackets
/// ("Мама (NL4)"): the same device now lives on every node of its env, so a bare
/// device name would produce a list of identical rows. ` · ` stays reserved for
/// the protocol suffix the client splits on, hence the brackets.
fn connection_title(
    node_label: &str,
    hostname: &str,
    address: &str,
    country: &str,
    personal: bool,
    conn_label: Option<&str>,
) -> String {
    let node_label = node_label.trim();
    let device = conn_label.map(str::trim).filter(|l| !l.is_empty());

    // A personal node whose label is just its host has no name of its own: the
    // device is the only meaningful title, the country is the fallback.
    if personal && label_is_host(node_label, hostname, address) {
        if let Some(label) = device {
            return label.to_string();
        }
        let code = country.trim();
        if code.is_empty() {
            return "Private".to_string();
        }
        return format!("{}| Private", code.to_uppercase());
    }

    let node_title = if node_label.is_empty() {
        country.trim().to_uppercase()
    } else {
        node_label.to_string()
    };

    match device {
        Some(label) if node_title.is_empty() || node_title == label => label.to_string(),
        Some(label) => format!("{} ({})", label, node_title),
        None => node_title,
    }
}

pub(crate) fn config_rev(node: &fcore::Node, tag: Tag, ips: &[String]) -> Option<String> {
    let inbound = node.inbounds.get(&tag)?;
    let host = if tag == Tag::VlessXhttpCdn {
        inbound
            .stream_settings
            .as_ref()
            .and_then(|s| s.tls_settings.as_ref())
            .and_then(|t| t.server_name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| node.connection_host())
    } else {
        node.connection_host()
    };
    let body = serde_json::json!({
        "host": host,
        "address": node.address,
        "port": inbound.port,
        "node_ips": ips,
        "dns": ["1.1.1.1", "1.0.0.1"],
        "server": server_material(inbound),
    });
    let mut hasher = Sha256::new();
    hasher.update(body.to_string().as_bytes());
    Some(format!("{:x}", hasher.finalize()))
}

/// Версия конфига карточки пула.
///
/// Хеширует **все** точки входа, а не одну ноду-представителя: у членов пула
/// порты могут быть разными, и смена порта у любого из них обязана поднять
/// версию, иначе клиент останется на старом адресе и будет дозваниваться не
/// туда.
///
/// Серверный материал входит в хеш наравне с адресами — ровно как в
/// `config_rev`. Без него смена ключа или obfuscation не поднимала бы версию, и
/// клиенты продолжали бы работать на старом конфиге, пока их не переустановят.
fn config_rev_pool(members: &[&fcore::Node], tag: Tag, ports: &BTreeMap<String, u16>) -> String {
    let servers: Vec<serde_json::Value> = members
        .iter()
        .filter_map(|node| {
            let inbound = node.inbounds.get(&tag)?;
            Some(serde_json::json!({
                "uuid": node.uuid,
                "host": node.connection_host(),
                "server": server_material(inbound),
            }))
        })
        .collect();
    let body = serde_json::json!({
        "ports": ports,
        "servers": servers,
    });
    let mut hasher = Sha256::new();
    hasher.update(body.to_string().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn server_material(inbound: &fcore::Inbound) -> serde_json::Value {
    let awg = inbound.awg.as_ref().map(|awg| {
        serde_json::json!({
            "pub": awg.interface.private_key.pubkey().ok(),
            "port": awg.interface.listen_port,
            "mtu": awg.interface.mtu,
            "dns": awg.interface.dns,
            "keepalive": awg.keepalive.unwrap_or(25),
            "obf": serde_json::to_value(&awg.obfuscation).unwrap_or(serde_json::Value::Null),
        })
    });
    let wg = inbound.wg.as_ref().map(|wg| {
        serde_json::json!({
            "pub": wg.keys.pubkey().ok(),
            "port": wg.port,
            "keepalive": wg.keepalive.unwrap_or(25),
            "dns": ["1.1.1.1", "1.0.0.1"],
        })
    });
    let h2 = inbound.h2.as_ref().map(|h2| {
        serde_json::json!({
            "host": h2.host,
            "sni": h2.sni,
            "insecure": h2.insecure,
            "obfs": h2.obfs,
            "alpn": h2.alpn,
            "up": h2.up_mbps,
            "down": h2.down_mbps,
        })
    });
    let stream = inbound.stream_settings.as_ref().map(|s| {
        let reality = s.reality_settings.as_ref().map(|r| {
            serde_json::json!({
                "server_names": r.server_names,
                "public_key": r.public_key,
                "short_ids": r.short_ids,
            })
        });
        serde_json::json!({
            "network": s.network,
            "reality": reality,
            "grpc": s.grpc_settings,
            "xhttp": s.xhttp_settings,
            "tls_name": s.tls_settings.as_ref().and_then(|t| t.server_name.clone()),
        })
    });
    serde_json::json!({
        "awg": awg,
        "wg": wg,
        "h2": h2,
        "stream": stream,
        "mtproto": inbound.mtproto_secret,
    })
}

/// Returns the list of connections for the given protocol.
/// For each online node with the required inbound, finds a matching connection of the subscription.
/// A node-pinned conn (`conn_nodes`) matches only its own node.
fn entry_ips(node: &fcore::Node) -> Vec<String> {
    node.node_ips
        .as_ref()
        .map(|ips| ips.iter().map(|ip| ip.to_string()).collect())
        .filter(|ips: &Vec<String>| !ips.is_empty())
        .unwrap_or_else(|| vec![node.address.to_string()])
}

/// Addresses of one node with their port.
///
/// `node_ips` are addresses of one interface, so they share the port. An empty
/// list means the single `address`.
fn node_ports_of_node(node: &fcore::Node, port: u16) -> Vec<(String, u16)> {
    let ips: Vec<String> = node
        .node_ips
        .as_ref()
        .map(|ips| ips.iter().map(|ip| ip.to_string()).collect())
        .filter(|ips: &Vec<String>| !ips.is_empty())
        .unwrap_or_else(|| vec![node.address.to_string()]);
    ips.into_iter().map(|ip| (ip, port)).collect()
}

/// Every entry of the pool: one record per IP of each online node.
///
/// The order is deterministic (node uuid, then `node_ips` order) so the response
/// and the version hash do not jitter between restarts on a HashMap walk.
pub(crate) fn pooled_node_ports<N: NodeStorageOperations>(
    nodes: &N,
    env: &fcore::Env,
    pool: &str,
    tag: Tag,
) -> BTreeMap<String, u16> {
    let members = pool_members(nodes, env, pool);
    node_ports_of_members(&members, tag)
}

/// Online members of the pool, in a stable order (by uuid).
///
/// A separate function keeps `node_ips`, `node_ports` and `node_weights` over
/// the same member set: otherwise an old client (it sees only `node_ips`) and a
/// new one (sees both) would get different pools.
fn pool_members<'a, N: NodeStorageOperations>(
    nodes: &'a N,
    env: &fcore::Env,
    pool: &str,
) -> Vec<&'a fcore::Node> {
    let mut members: Vec<&fcore::Node> = nodes
        .iter_nodes()
        .map(|(_, n)| n)
        .filter(|n| {
            n.status == NodeStatus::Online
                && &n.env == env
                && normalized_pool(n.pool.clone()).as_deref() == Some(pool)
        })
        .collect();
    members.sort_by_key(|n| n.uuid);
    members
}

/// The pure part of `pooled_node_ports`: the member set is already filtered and
/// sorted.
///
/// A node without the required inbound is not added: it cannot serve that
/// protocol, and its address in the pool card would be a dead entry point.
fn node_ports_of_members(members: &[&fcore::Node], tag: Tag) -> BTreeMap<String, u16> {
    let mut out: BTreeMap<String, u16> = BTreeMap::new();
    for node in members {
        let Some(inbound) = node.inbounds.get(&tag) else {
            continue;
        };
        for (ip, port) in node_ports_of_node(node, inbound.port) {
            out.insert(ip, port);
        }
    }
    out
}

/// Weights for an already collected set of entry addresses.
///
/// Computed over the same IP list as `node_ports`: otherwise the client would
/// get a weight for an address that is not in the pool, or lose an address
/// without one. An address with no weight weighs `DEFAULT_NODE_WEIGHT`.
fn node_weights_of_ips(
    members: &[&fcore::Node],
    ips: &BTreeMap<String, u16>,
) -> BTreeMap<String, i32> {
    ips.keys()
        .map(|ip| {
            let weight = members
                .iter()
                .find(|node| node.entry_ips().iter().any(|own| own == ip))
                .map(|node| node.weight_for(ip))
                .unwrap_or(DEFAULT_NODE_WEIGHT);
            (ip.clone(), weight)
        })
        .collect()
}

/// Weights of a single-node entry: sent only when the node has several
/// addresses or explicit weights — otherwise it is noise in the response.
fn gateway_node_weights(node: &fcore::Node) -> Option<BTreeMap<String, i32>> {
    if node.entry_ips().len() < 2 && node.node_weights.is_empty() {
        return None;
    }
    Some(node.weights_by_ip())
}

/// Backwards compatibility: addresses only, no ports.
fn connections_for_protocol<N, C>(
    nodes: &N,
    conn_nodes: &std::collections::HashMap<uuid::Uuid, uuid::Uuid>,
    conn_labels: &std::collections::HashMap<uuid::Uuid, String>,
    protocol: &str,
    conns: Option<&[(uuid::Uuid, C)]>,
    collapse_pools: bool,
) -> Vec<GatewayConnection>
where
    N: NodeStorageOperations,
    C: ConnectionApiOperations + ConnectionBaseOperations,
{
    let mut result: Vec<GatewayConnection> = Vec::new();
    let mut pool_index: std::collections::HashMap<(uuid::Uuid, fcore::Tag, String), usize> =
        std::collections::HashMap::new();
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
                // A node-pinned conn exists only on its node.
                if !pinned_to(conn_nodes, conn_id, &node.uuid) {
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

                if collapse_pools {
                    if let Some(pool) = normalized_pool(node.pool.clone()) {
                        let key = (*conn_id, tag, pool.clone());
                        if pool_index.contains_key(&key) {
                            continue;
                        }
                        pool_index.insert(key, result.len());
                        // Same rule as single nodes: a named device prefixes the
                        // entry, the pool name tells the nodes apart.
                        let pool_title = match conn_labels
                            .get(conn_id)
                            .map(|l| l.trim())
                            .filter(|l| !l.is_empty())
                        {
                            Some(device) => format!("{device} ({pool})"),
                            None => pool.clone(),
                        };
                        result.push(GatewayConnection {
                            connection_uuid: *conn_id,
                            node_id: node.uuid,
                            country_code: code.clone(),
                            country_name: code.clone(),
                            service_protocol: proto_label(tag).to_string(),
                            env: conn.get_env().to_string(),
                            connection_label: format!("{pool_title} · {}", inbound_label(tag)),
                            node_ips: None,
                            node_ports: None,
                            pool: Some(pool.clone()),
                            pool_translation: node.pool_translation.clone(),
                            weight: node.max_weight(),
                            node_weights: None,
                            config_rev: None,
                        });
                        continue;
                    }
                }

                let title = connection_title(
                    &node.label,
                    &node.hostname,
                    &node.address.to_string(),
                    &code,
                    node.env.is_personal(),
                    conn_labels.get(conn_id).map(String::as_str),
                );
                let label = format!("{title} · {}", inbound_label(tag));
                let ips: Vec<String> = node
                    .node_ips
                    .as_ref()
                    .map(|ips| ips.iter().map(|ip| ip.to_string()).collect())
                    .unwrap_or_default();
                result.push(GatewayConnection {
                    connection_uuid: *conn_id,
                    node_id: node.uuid,
                    country_code: code.clone(),
                    country_name: code.clone(),
                    service_protocol: proto_label(tag).to_string(),
                    env: conn.get_env().to_string(),
                    connection_label: label,
                    node_ips: if ips.is_empty() {
                        None
                    } else {
                        Some(ips.clone())
                    },
                    // Одиночная нода: все её адреса — один интерфейс, порт
                    // равен порту протокола из конфига. Карта была бы чистым
                    // дублированием, поэтому её здесь нет.
                    node_ports: None,
                    pool: node.pool.clone(),
                    pool_translation: node.pool_translation.clone(),
                    weight: node.max_weight(),
                    node_weights: gateway_node_weights(node),
                    config_rev: config_rev(node, tag, &ips),
                });
            }
        }
    }
    for ((_, tag, pool), idx) in &pool_index {
        let env = fcore::Env::from(result[*idx].env.as_str());
        // Одна карта на оба поля: `node_ips` — её ключи, `node_ports` — она сама.
        let ports = pooled_node_ports(nodes, &env, pool, *tag);
        if !ports.is_empty() {
            let ips: Vec<String> = ports.keys().cloned().collect();
            result[*idx].node_ips = Some(ips);
            result[*idx].node_ports = Some(ports.clone());
            let members = pool_members(nodes, &env, pool);
            // Weights are computed over the same address set as the ports.
            let weights = node_weights_of_ips(&members, &ports);
            result[*idx].weight = weights
                .values()
                .copied()
                .max()
                .unwrap_or(DEFAULT_NODE_WEIGHT);
            result[*idx].node_weights = Some(weights);
            result[*idx].config_rev = Some(config_rev_pool(&members, *tag, &ports));
        }
    }
    if result.is_empty() {
        result = vec![GatewayConnection {
            connection_uuid: uuid::Uuid::nil(),
            node_id: uuid::Uuid::nil(),
            country_code: String::new(),
            country_name: "All countries".to_string(),
            service_protocol: String::new(),
            env: String::new(),
            connection_label: "All countries".to_string(),
            node_ips: None,
            node_ports: None,
            pool: None,
            pool_translation: None,
            weight: DEFAULT_NODE_WEIGHT,
            node_weights: None,
            config_rev: None,
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
                pool: conn.pool.clone(),
                pool_translation: conn.pool_translation.clone(),
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
    let persistent_keepalive = ini.get("PersistentKeepalive").unwrap_or(&"25");

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

/// Builds the Amnezia server config for plain WireGuard: a wg-quick INI
/// without junk parameters, delivered in the `amnezia-wireguard` container.
/// The client generates its own keypair; the INI carries the
/// `$WIREGUARD_CLIENT_PRIVATE_KEY` placeholder substituted on the client side.
fn build_wg_server_config(
    inbound: &fcore::Inbound,
    conn: &Connection,
    client_pub_key: &str,
    host: &str,
) -> Result<serde_json::Value, fcore::Error> {
    let wg = inbound
        .wg
        .as_ref()
        .ok_or(fcore::Error::Custom("WireGuard settings missing".into()))?;
    let param = conn.get_wireguard().ok_or(fcore::Error::Custom(
        "Connection has no WireGuard params".into(),
    ))?;

    let server_pub_key = wg.keys.pubkey()?;
    let client_ip = param.address.address.to_string();
    // Real server-generated private key (AGW envelope is encrypted).
    let client_priv_key = &param.keys.privkey;
    let keepalive = wg.keepalive.unwrap_or(25);

    let ini = format!(
        r#"[Interface]
PrivateKey = {client_priv_key}
Address = {address}
DNS = 1.1.1.1, 1.0.0.1
MTU = 1420

[Peer]
PublicKey = {server_pub_key}
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint = {host}:{port}
PersistentKeepalive = {keepalive}
"#,
        address = param.address,
        server_pub_key = server_pub_key,
        host = host,
        port = inbound.port,
    );

    let last_config = serde_json::json!({
        "client_priv_key": client_priv_key,
        "client_pub_key": client_pub_key,
        "server_pub_key": server_pub_key,
        "psk_key": "",
        "client_ip": client_ip,
        "hostName": host,
        "port": inbound.port,
        "mtu": "1420",
        "persistent_keep_alive": keepalive.to_string(),
    });

    Ok(serde_json::json!({
        "config_version": 2,
        "defaultContainer": "amnezia-wireguard",
        "description": "FRKN WireGuard",
        "dns1": "1.1.1.1",
        "dns2": "1.0.0.1",
        "hostName": host,
        "name": "FRKN",
        "containers": [
            {
                "container": "amnezia-wireguard",
                "amnezia-wireguard": {
                    "config": ini,
                    "isThirdPartyConfig": true,
                    "last_config": last_config.to_string()
                }
            }
        ]
    }))
}

/// Builds the Amnezia server config for Hysteria2 (xray outbound, same
/// container shape and delivery as VLESS).
fn build_h2_server_config(
    inbound: &fcore::Inbound,
    host: &str,
    token: &uuid::Uuid,
) -> Result<serde_json::Value, fcore::Error> {
    let h2 = inbound
        .h2
        .as_ref()
        .ok_or(fcore::Error::Custom("Hysteria2 settings missing".into()))?;

    let server_name = h2.sni.clone().unwrap_or_else(|| h2.host.clone());

    let mut settings = serde_json::json!({
        "servers": [{
            "address": host,
            "port": inbound.port,
            "password": token.to_string(),
            "serverName": server_name,
        }],
    });
    if let Some(obfs) = &h2.obfs {
        settings["obfs"] = serde_json::json!({
            "type": obfs.r#type,
            "password": obfs.password,
        });
    }
    if let Some(up) = h2.up_mbps {
        settings["up_mbps"] = serde_json::json!(up);
    }
    if let Some(down) = h2.down_mbps {
        settings["down_mbps"] = serde_json::json!(down);
    }

    let alpn = h2.alpn.clone().unwrap_or_else(|| vec!["h3".to_string()]);

    let outbound = serde_json::json!({
        "outbounds": [{
            "protocol": "hysteria2",
            "settings": settings,
            "streamSettings": {
                "network": "udp",
                "security": "tls",
                "tlsSettings": {
                    "serverName": server_name,
                    "fingerprint": "chrome",
                    "alpn": alpn,
                    "allowInsecure": h2.insecure,
                },
            },
        }],
    });

    Ok(serde_json::json!({
        "containers": [
            {
                "container": "amnezia-xray",
                "xray": {
                    "config": outbound.to_string(),
                    "last_config": outbound.to_string(),
                    "isThirdPartyConfig": true
                }
            }
        ],
        "defaultContainer": "amnezia-xray",
        "dns1": "1.1.1.1",
        "dns2": "1.0.0.1",
        "hostName": host,
        "description": "FRKN Hysteria2",
        "name": "FRKN",
        "config_version": 2
    }))
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
    traffic_mode_enabled: bool,
    metered_conns: Vec<String>,
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
    // A `conn` share token authorizes /v1/config and nothing else. A
    // shared-subscription token (`sub`) is a subscription credential: the
    // importer needs this list to build one card per server, so it is
    // served here, scoped to the token's own connection (env + protocol).
    let share_row = match req
        .auth_data
        .as_ref()
        .and_then(|a| a.get("share_token"))
        .and_then(|v| v.as_str())
    {
        Some(raw) => {
            let Some(token) = share::normalize_share_token(raw) else {
                return Ok(http::not_found("share_not_found").into_response());
            };
            match memory.db.share().get(&token).await {
                Ok(Some(row)) if row.revoked_at.is_none() => Some(row),
                Ok(_) => return Ok(http::not_found("share_not_found").into_response()),
                Err(e) => {
                    tracing::error!("services: share lookup failed: {}", e);
                    return Ok(http::internal_error("share lookup failed").into_response());
                }
            }
        }
        None => None,
    };
    if let Some(row) = &share_row {
        if row.kind != share::SHARE_KIND_SUB {
            return Ok(share::forbidden("share_token is only valid for /v1/config"));
        }
    }

    let mem = memory.memory.read().await;

    // A shared subscription is identified by its token, not by auth_data's
    // subscription id; its child connection defines the env+protocol scope.
    let sub_id = match &share_row {
        Some(row) => Some(row.subscription_id),
        None => req.auth_data.as_ref().and_then(extract_subscription_id),
    };
    let sub = sub_id.and_then(|sub_id| mem.subscriptions.find_by_id(&sub_id));

    // Expired/revoked subscription: paywall signal, same as /v1/config.
    let mut traffic_mode = false;
    if let (Some(sub), Some(sub_id)) = (sub, sub_id) {
        match resolve_serving_access(
            &memory.db,
            sub_id,
            serving_access(sub, traffic_mode_enabled),
        )
        .await
        {
            ServingAccess::Expired => {
                return Ok(subscription_expired_response(sub.expires_at()));
            }
            ServingAccess::TrafficMode => traffic_mode = true,
            ServingAccess::Full => {}
            ServingAccess::CheckBalance(_) => unreachable!("resolved above"),
        }
    }

    let end_date = sub
        .as_ref()
        .and_then(|sub| sub.expires_at().map(|d| d.to_rfc3339()));
    let conns = match &share_row {
        Some(row) => mem
            .connections
            .get(&row.connection_id)
            .filter(|c| !c.get_deleted())
            .map(|c| vec![(row.connection_id, c.clone())]),
        None => sub_id
            .and_then(|sub_id| mem.connections.get_by_subscription_id(&sub_id))
            .map(|cs| {
                // Share-issued child connections are not the owner's devices.
                cs.into_iter()
                    .filter(|(conn_id, _)| {
                        !share::is_share_connection(&mem.share_conns, conn_id)
                            && main_feed_connection_visible(
                                &mem.main_feed_excluded_conns,
                                conn_id,
                                true,
                            )
                    })
                    .collect::<Vec<_>>()
            }),
    };
    let conns_slice = conns.as_deref();

    let vless_connections = connections_for_protocol(
        &mem.nodes,
        &mem.conn_nodes,
        &mem.conn_labels,
        "vless",
        conns_slice,
        !req.pool_off,
    );
    let awg_connections = connections_for_protocol(
        &mem.nodes,
        &mem.conn_nodes,
        &mem.conn_labels,
        "awg",
        conns_slice,
        !req.pool_off,
    );

    // One merged service: the client must not offer a protocol choice at purchase.
    let mut connections = vless_connections;
    connections.extend(awg_connections);
    // Hysteria2 has no per-connection traffic accounting: hidden in traffic mode.
    if !traffic_mode || Tag::Hysteria2.is_metered(&metered_conns) {
        connections.extend(connections_for_protocol(
            &mem.nodes,
            &mem.conn_nodes,
            &mem.conn_labels,
            "hysteria2",
            conns_slice,
            !req.pool_off,
        ));
    }
    connections.extend(connections_for_protocol(
        &mem.nodes,
        &mem.conn_nodes,
        &mem.conn_labels,
        "awg-mobile",
        conns_slice,
        !req.pool_off,
    ));
    connections.extend(connections_for_protocol(
        &mem.nodes,
        &mem.conn_nodes,
        &mem.conn_labels,
        "wireguard",
        conns_slice,
        !req.pool_off,
    ));
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
    traffic_mode_enabled: bool,
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
    // A share token authorizes /v1/config and nothing else.
    if share::auth_data_has_share_token(&req.auth_data) {
        return Ok(share::forbidden("share_token is only valid for /v1/config"));
    }

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

    // Informational endpoint: traffic mode counts as active, only expired blocks.
    let access = resolve_serving_access(
        &memory.db,
        sub_id,
        serving_access(sub, traffic_mode_enabled),
    )
    .await;
    if access == ServingAccess::Expired {
        return Ok(http::not_found("Subscription expired").into_response());
    }

    let conns = mem.connections.get_by_subscription_id(&sub_id);
    // Share-issued child connections are not the owner's devices.
    let active_devices = conns
        .as_ref()
        .map(|cs| {
            cs.iter()
                .filter(|(conn_id, _)| !share::is_share_connection(&mem.share_conns, conn_id))
                .count() as i64
        })
        .unwrap_or(0);

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
        supported_protocols: vec![
            "vless".to_string(),
            "awg".to_string(),
            "awg-mobile".to_string(),
            "wireguard".to_string(),
        ],
        available_countries: vec![GatewayCountry {
            country_code: String::new(),
            country_name: "All countries".to_string(),
            connection_uuid: None,
            connection_label: "All countries".to_string(),
            pool: None,
            pool_translation: None,
        }],
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
    })
    .into_response())
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

/// 402 Payment Required for an expired/revoked subscription — the body is
/// encrypted by the envelope like a normal 200 response, so the client can
/// show a paywall instead of a generic error.
fn subscription_expired_response(
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> warp::reply::Response {
    let end_date = expires_at.map(|d| d.to_rfc3339()).unwrap_or_default();
    let message = if end_date.is_empty() {
        "Subscription expired. Renew to continue.".to_string()
    } else {
        format!("Subscription expired on {}. Renew to continue.", end_date)
    };

    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({
            "error": "subscription_expired",
            "end_date": end_date,
            "message": message,
        })),
        warp::http::StatusCode::PAYMENT_REQUIRED,
    )
    .into_response()
}

/// Parameters for building a gateway VPN config, shared by
/// `gateway_config_handler` and `gateway_subscriptions_handler`.
pub struct GatewayConfigParams<'a> {
    pub service_protocol: &'a str,
    pub service_type: &'a str,
    pub user_country_code: Option<&'a str>,
    pub server_country_code: Option<&'a str>,
    pub connection_id: Option<uuid::Uuid>,
    pub node_id: Option<uuid::Uuid>,
}

/// Builds the gateway config for an active subscription: picks a matching
/// (connection, online node) pair and renders the Amnezia server config.
///
/// Returns `Err(response)` with a ready error response (4xx) on failure.
pub async fn build_gateway_config_response<N, C, S>(
    memory: &MemSync<N, C, S>,
    sub_id: &uuid::Uuid,
    params: &GatewayConfigParams<'_>,
    traffic_mode_enabled: bool,
    metered_conns: &[String],
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

    let access = resolve_serving_access(
        &memory.db,
        *sub_id,
        serving_access(sub, traffic_mode_enabled),
    )
    .await;
    let traffic_mode = match access {
        ServingAccess::Expired => {
            return Err(subscription_expired_response(sub.expires_at()));
        }
        ServingAccess::TrafficMode => true,
        ServingAccess::Full => false,
        ServingAccess::CheckBalance(_) => unreachable!("resolved above"),
    };

    let conns = match mem.connections.get_by_subscription_id(sub_id) {
        Some(c) => c,
        None => return Err(http::not_found("No connections").into_response()),
    };

    // No geoip on the gateway yet: an absent country simply disables the filter.
    let target_country = params
        .server_country_code
        .or(params.user_country_code)
        .unwrap_or("");

    let mut found_conn = None;
    let mut found_node = None;
    let mut found_conn_id = None;

    for (conn_id, conn) in conns {
        if conn.get_deleted() {
            continue;
        }
        if !main_feed_connection_visible(
            &mem.main_feed_excluded_conns,
            &conn_id,
            params.connection_id.is_none(),
        ) {
            continue;
        }
        let conn_tag = conn.get_proto().proto();
        // Traffic mode: protocols without per-connection accounting are unservable.
        if traffic_mode && !conn_tag.is_metered(metered_conns) {
            continue;
        }
        // An explicitly requested connection decides its own protocol — the
        // merged "FRKN Premium" card carries connections of both protocols.
        if let Some(requested_id) = params.connection_id {
            if conn_id != requested_id {
                continue;
            }
        } else if !proto_matches(conn_tag, params.service_protocol) {
            continue;
        }

        if let Some(nodes) = mem.nodes.get_by_env(&conn.get_env()) {
            for node in nodes {
                // A node-pinned conn exists only on its node: never render
                // its config for another node where the peer is absent.
                if !pinned_to(&mem.conn_nodes, &conn_id, &node.uuid) {
                    continue;
                }
                // A pinned node_id selects exactly that node; a mismatch is
                // an explicit error, not a silent fallback to another node.
                if let Some(wanted) = params.node_id {
                    if node.uuid != wanted {
                        continue;
                    }
                }
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
            if params.node_id.is_some() {
                return Err(http::not_found("node_not_found").into_response());
            }
            return Err(http::not_found("No suitable connection/node found").into_response());
        }
    };

    let pinned = mem.conn_nodes.contains_key(&conn_id);
    let pool = if pinned {
        None
    } else {
        normalized_pool(node.pool.clone())
    };
    let service_name = if let Some(pool) = &pool {
        pool.clone()
    } else {
        connection_title(
            &node.label,
            &node.hostname,
            &node.address.to_string(),
            &node.country,
            node.env.is_personal(),
            mem.conn_labels.get(&conn_id).map(String::as_str),
        )
    };
    let conn_tag_for_ports = conn.get_proto().proto();
    // `node_ips` и `node_ports` — из одной карты: адреса и их порты.
    let (node_ips, node_ports) = if let Some(pool) = &pool {
        let ports = pooled_node_ports(&mem.nodes, &node.env, pool, conn_tag_for_ports);
        if ports.is_empty() {
            (None, None)
        } else {
            let ips: Vec<String> = ports.keys().cloned().collect();
            (Some(ips), Some(ports))
        }
    } else {
        let ips = node
            .node_ips
            .as_ref()
            .map(|ips| ips.iter().map(|ip| ip.to_string()).collect());
        // Одиночная нода: порт один и он уже в конфиге — карта не нужна.
        (ips, None)
    };
    let config_rev = config_rev(
        &node,
        conn.get_proto().proto(),
        node_ips.as_deref().unwrap_or(&[]),
    );

    // From here on only owned clones are used; release the read lock so the
    // WireGuard branch can take a write lock for peer registration.
    drop(mem);

    // Build the config for the protocol of the chosen connection, not of the
    // service card: an explicitly requested connection may be of either type.
    let conn_tag = conn.get_proto().proto();
    let server_config_json = if conn_tag == Tag::Hysteria2 {
        let inbound = node.inbounds.get(&conn_tag).ok_or_else(|| {
            http::internal_error("Node has no matching Hysteria2 inbound").into_response()
        })?;
        let token = conn.get_token().ok_or_else(|| {
            http::internal_error("Connection has no Hysteria2 token").into_response()
        })?;
        let host = node.connection_host();
        build_h2_server_config(inbound, &host, &token).map_err(|e| {
            http::internal_error(&format!("Failed to build Hysteria2 config: {e}")).into_response()
        })?
    } else if conn_tag == Tag::Wireguard {
        let inbound = node.inbounds.get(&conn_tag).ok_or_else(|| {
            http::internal_error("Node has no matching WireGuard inbound").into_response()
        })?;
        let host = node.connection_host();

        // AWG-style server-side keypair (per the client spec): the real
        // client private key travels in the encrypted envelope; the node
        // already knows the peer from the regular connection sync.
        let param = conn.get_wireguard().ok_or_else(|| {
            http::internal_error("Connection has no WireGuard params").into_response()
        })?;
        let client_pub_key = param.keys.pubkey().map_err(|_| {
            http::internal_error("Failed to derive WireGuard public key").into_response()
        })?;

        build_wg_server_config(inbound, &conn, &client_pub_key, &host).map_err(|e| {
            http::internal_error(&format!("Failed to build WireGuard config: {e}")).into_response()
        })?
    } else if matches!(conn_tag, Tag::AmneziaWg | Tag::AmneziaWgMobile) {
        let inbound = node.inbounds.get(&conn_tag).ok_or_else(|| {
            http::internal_error("Node has no matching AmneziaWG inbound").into_response()
        })?;
        let host = node.connection_host();
        let link = inbound
            .create_link(&conn_id, &conn, &node.hostname, &host, &node.label)
            .map_err(|_| http::internal_error("Failed to build AWG link").into_response())?;

        let awg_param = conn.get_amneziawg().ok_or_else(|| {
            http::internal_error("Connection has no AmneziaWG params").into_response()
        })?;
        let client_priv_key = awg_param.keys.privkey.clone();
        let client_pub_key = awg_param.keys.pubkey().map_err(|_| {
            http::internal_error("Failed to derive AmneziaWG public key").into_response()
        })?;

        let port = inbound.port;

        build_awg_server_config(&link, &client_priv_key, &client_pub_key, &host, port)
    } else {
        // Match the inbound by the connection's exact tag (like the
        // subscription link handler does). No fallback: serving a config
        // from a different inbound means a broken host.
        let inbound = node.inbounds.get(&conn_tag).ok_or_else(|| {
            http::internal_error("Node has no matching VLESS inbound").into_response()
        })?;

        // The user id is always the connection's uuid (as in the
        // subscription link handler) — never the client's public_key,
        // which may be a fresh random uuid per request.
        let host = node.connection_host();
        build_vless_server_config(inbound, &conn_id, &host)
            .map_err(|_| http::internal_error("Failed to build VLESS config").into_response())?
    };

    // AWG and plain WireGuard are delivered gzipped (base64 STANDARD);
    // vless and hysteria2 go as plain JSON with base64url (no padding).
    let is_gzipped = matches!(
        conn_tag,
        Tag::AmneziaWg | Tag::AmneziaWgMobile | Tag::Wireguard
    );
    let config_bytes = if is_gzipped {
        gzip_json(&server_config_json)
            .map_err(|_| http::internal_error("Failed to gzip config").into_response())?
    } else {
        server_config_json.to_string().into_bytes()
    };
    let config_b64 = if is_gzipped {
        base64::engine::general_purpose::STANDARD.encode(&config_bytes)
    } else {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&config_bytes)
    };

    Ok(GatewayConfigResponse {
        config: config_b64,
        supported_protocols: vec![
            "vless".to_string(),
            "awg".to_string(),
            "awg-mobile".to_string(),
            "hysteria2".to_string(),
            "wireguard".to_string(),
        ],
        service_info: serde_json::json!({
            "name": service_name,
            "type": params.service_type
        }),
        api_config: serde_json::json!({
            "service_type": params.service_type,
            // Report the protocol of the config we actually built — with the
            // merged Premium card it can differ from the request.
            "service_protocol": proto_label(conn_tag),
            // Keep the field a string (empty when unknown) — the client
            // parses api_config strictly.
            "user_country_code": params.user_country_code.unwrap_or(""),
            "server_country_code": params.server_country_code
        }),
        share_label: None,
        country_code: None,
        country_name: None,
        service_protocol: None,
        service_type: None,
        node_ips,
        node_ports,
        config_rev,
    })
}

pub async fn gateway_config_handler<N, C, S>(
    req: GatewayConfigRequest,
    memory: MemSync<N, C, S>,
    traffic_mode_enabled: bool,
    metered_conns: Vec<String>,
    remote: Option<std::net::SocketAddr>,
    x_forwarded_for: Option<String>,
    rate_limiter: std::sync::Arc<share::RateLimiter>,
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
    // Share-token auth is a separate, tightly scoped branch: the config is
    // pinned to the token's child connection and nothing else.
    if let Some(raw_token) = req.auth_data.get("share_token").and_then(|v| v.as_str()) {
        let raw_token = raw_token.to_string();
        return Ok(share::gateway_share_config_handler(
            req,
            &raw_token,
            memory,
            remote,
            x_forwarded_for,
            rate_limiter,
        )
        .await);
    }

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

    // Owner path: both fields are required (share_token auth returns above
    // and never needs them).
    let (Some(service_type), Some(service_protocol)) =
        (req.service_type.as_deref(), req.service_protocol.as_deref())
    else {
        return Ok(warp::reply::with_status(
            "Missing service_type/service_protocol",
            warp::http::StatusCode::BAD_REQUEST,
        )
        .into_response());
    };

    // Событие подключения пишем ДО сборки ответа: клиент ждёт обычный
    // конфиг, и сбой записи не должен его ломать (ошибка только в журнал).
    if req.is_connect_event {
        record_connect_event(&req, &sub_id, service_protocol, &memory, remote, x_forwarded_for.as_deref()).await;
    }

    let params = GatewayConfigParams {
        service_protocol,
        service_type,
        user_country_code: req.user_country_code.as_deref(),
        server_country_code: req.server_country_code.as_deref(),
        connection_id: req.connection_id,
        node_id: req.node_id,
    };

    match build_gateway_config_response(
        &memory,
        &sub_id,
        &params,
        traffic_mode_enabled,
        &metered_conns,
    )
    .await
    {
        Ok(response) => Ok(warp::reply::json(&response).into_response()),
        Err(response) => Ok(response),
    }
}

/// Пишет событие подключения. Best-effort: любые ошибки — в журнал,
/// ответ клиенту не меняется.
async fn record_connect_event<N, C, S>(
    req: &GatewayConfigRequest,
    sub_id: &uuid::Uuid,
    protocol: &str,
    memory: &MemSync<N, C, S>,
    remote: Option<std::net::SocketAddr>,
    x_forwarded_for: Option<&str>,
) where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq,
{
    let endpoint = req
        .endpoint
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<std::net::IpAddr>().ok());
    let support_tag = req
        .support_tag
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let platform = match req.service_type.as_deref() {
        Some(t) if t.contains("ios") => "ios",
        _ => req.os_version.split(' ').next().unwrap_or("unknown"),
    };
    // Страна выхода по адресу: у нас уже есть GeoIP на api.
    let country = endpoint.and_then(|ip| fcore::geoip::lookup_full(ip).country);

    let event = crate::postgres::connect::ConnectEvent {
        subscription_id: Some(*sub_id),
        node_id: req.node_id,
        connection_id: req.connection_id,
        protocol: protocol.to_string(),
        endpoint,
        endpoint_port: req.endpoint_port.map(|p| p as i32),
        support_tag,
        app_version: req.app_version.clone(),
        platform: platform.to_string(),
        country,
    };
    let _ = (remote, x_forwarded_for);

    if let Err(err) = memory.db.connect().record(&event).await {
        tracing::warn!(
            event = "connect_event_failed",
            error = %err,
            endpoint = ?endpoint,
            "не удалось записать событие подключения"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcore::H2Settings;
    use fcore::Tag;
    use fcore::{Env, IpAddrMask, WireguardSettings};
    use fcore::{Proto, WgKeys, WgParam};

    fn h2_inbound() -> fcore::Inbound {
        fcore::Inbound {
            tag: Tag::Hysteria2,
            port: 443,
            stream_settings: None,
            wg: None,
            awg: None,
            mtproto_secret: None,
            h2: Some(H2Settings {
                host: "hy2.example.com".to_string(),
                port: 443,
                sni: None,
                insecure: false,
                obfs: None,
                alpn: None,
                up_mbps: None,
                down_mbps: None,
                auth_info: None,
            }),
        }
    }

    fn wg_inbound() -> fcore::Inbound {
        fcore::Inbound {
            tag: Tag::Wireguard,
            port: 51820,
            stream_settings: None,
            wg: Some(WireguardSettings {
                interface: "wg0".to_string(),
                address: "10.8.1.1/24".parse::<IpAddrMask>().unwrap(),
                port: 51820,
                keys: WgKeys::default(),
                dns: vec![],
                keepalive: None,
            }),
            awg: None,
            mtproto_secret: None,
            h2: None,
        }
    }

    fn wg_connection(param: WgParam) -> Connection {
        Connection::new(&Env::Production, None, Proto::Wireguard { param }, None)
    }

    /// connect-event: новые поля читаются, старый запрос без них — валиден.
    /// Это и есть обещание совместимости из frkn-docs/api-connect-event.md.
    #[test]
    fn connect_event_fields_are_optional() {
        let base = serde_json::json!({
            "os_version": "26.6.2",
            "app_version": "4.8.14.79",
            "app_language": "ru",
            "installation_uuid": "023d804b-e677-446e-aba4-d02742fb0e3a",
            "service_type": "amnezia-premium",
            "service_protocol": "awg-mobile",
            "auth_data": {"api_key": "x"}
        });

        // обычный refresh конфига — событие не отправлено
        let plain: GatewayConfigRequest = serde_json::from_value(base.clone()).unwrap();
        assert!(!plain.is_connect_event);
        assert!(plain.endpoint.is_none());
        assert!(plain.endpoint_port.is_none());
        assert!(plain.support_tag.is_none());

        // событие подключения
        let mut with_event = base;
        with_event["is_connect_event"] = serde_json::json!(true);
        with_event["connection_id"] = serde_json::json!("504921fc-7201-4b5d-a40c-808570923bd3");
        with_event["endpoint"] = serde_json::json!("93.89.220.49");
        with_event["endpoint_port"] = serde_json::json!(443);
        with_event["support_tag"] = serde_json::json!("023d804b-e677-446e-aba4-d02742fb0e3a");
        let ev: GatewayConfigRequest = serde_json::from_value(with_event).unwrap();
        assert!(ev.is_connect_event);
        assert_eq!(ev.endpoint.as_deref(), Some("93.89.220.49"));
        assert_eq!(ev.endpoint_port, Some(443));
        assert_eq!(
            ev.connection_id.unwrap().to_string(),
            "504921fc-7201-4b5d-a40c-808570923bd3"
        );
        assert_eq!(
            ev.support_tag.as_deref(),
            Some("023d804b-e677-446e-aba4-d02742fb0e3a")
        );
    }

    #[test]
    fn wg_config_shape() {
        let inbound = wg_inbound();
        let server_pub_key = inbound.wg.as_ref().unwrap().keys.pubkey().unwrap();
        let param_keys = WgKeys::default();
        let client_pub_key = param_keys.pubkey().unwrap();

        let param = WgParam {
            keys: param_keys.clone(),
            address: "10.8.1.5/32".parse::<IpAddrMask>().unwrap(),
        };
        let conn = wg_connection(param);

        let cfg = build_wg_server_config(&inbound, &conn, &client_pub_key, "203.0.113.7").unwrap();

        assert_eq!(cfg["defaultContainer"], "amnezia-wireguard");
        assert_eq!(cfg["containers"][0]["container"], "amnezia-wireguard");

        let inner = &cfg["containers"][0]["amnezia-wireguard"];
        assert_eq!(inner["isThirdPartyConfig"], true);

        // Plain WireGuard INI: real server-generated private key, no AWG junk.
        let ini = inner["config"].as_str().unwrap();
        assert!(ini.contains(&format!("PrivateKey = {}", param_keys.privkey)));
        assert!(ini.contains("Address = 10.8.1.5/32"));
        assert!(ini.contains(&format!("PublicKey = {}", server_pub_key)));
        assert!(ini.contains("Endpoint = 203.0.113.7:51820"));
        assert!(ini.contains("AllowedIPs = 0.0.0.0/0, ::/0"));
        assert!(ini.contains("PersistentKeepalive = 25"));
        for junk in ["Jc", "Jmin", "Jmax", "S1", "S2", "H1", "I1"] {
            // Junk params appear as their own INI lines ("S1 = ..."), not as
            // substrings (base64 keys may legitimately contain "S1" etc.).
            assert!(
                !ini.lines().any(|l| l.starts_with(&format!("{} =", junk))),
                "INI must not contain {} as a param line",
                junk
            );
        }

        // last_config is a JSON string with the fields the client expects.
        let last: serde_json::Value =
            serde_json::from_str(inner["last_config"].as_str().unwrap()).unwrap();
        assert_eq!(last["client_priv_key"], param_keys.privkey);
        assert_eq!(last["client_pub_key"], client_pub_key);
        assert_eq!(last["server_pub_key"], server_pub_key);
        assert_eq!(last["psk_key"], "");
        assert_eq!(last["client_ip"], "10.8.1.5");
        assert_eq!(last["hostName"], "203.0.113.7");
        assert_eq!(last["port"], 51820);
        assert_eq!(last["mtu"], "1420");
        assert_eq!(last["persistent_keep_alive"], "25");
    }

    #[test]
    fn h2_config_shape() {
        let token = uuid::Uuid::new_v4();
        let cfg = build_h2_server_config(&h2_inbound(), "203.0.113.7", &token).unwrap();

        assert_eq!(cfg["defaultContainer"], "amnezia-xray");
        assert_eq!(cfg["containers"][0]["container"], "amnezia-xray");
        assert_eq!(cfg["containers"][0]["xray"]["isThirdPartyConfig"], true);

        let inner = cfg["containers"][0]["xray"]["config"].as_str().unwrap();
        let outbound: serde_json::Value = serde_json::from_str(inner).unwrap();
        let ob = &outbound["outbounds"][0];
        assert_eq!(ob["protocol"], "hysteria2");
        let server = &ob["settings"]["servers"][0];
        assert_eq!(server["address"], "203.0.113.7");
        assert_eq!(server["port"], 443);
        assert_eq!(server["password"], token.to_string());
        // SNI falls back to the inbound host when not set
        assert_eq!(server["serverName"], "hy2.example.com");
        let tls = &ob["streamSettings"]["tlsSettings"];
        assert_eq!(tls["alpn"], serde_json::json!(["h3"]));
        assert_eq!(tls["allowInsecure"], false);
        assert_eq!(ob["streamSettings"]["network"], "udp");
    }

    #[test]
    fn share_config_request_may_omit_service_fields() {
        // Regression: the app's share import sends only auth_data +
        // installation_uuid + public_key — no service_type/service_protocol.
        let body = serde_json::json!({
            "os_version": "linux",
            "app_version": "4.8.14.33",
            "app_language": "ru",
            "installation_uuid": "11111111-2222-3333-4444-555555555555",
            "auth_data": {"share_token": "skcvac6pt2z0hh51"},
            "public_key": "abc"
        });
        let req: GatewayConfigRequest = serde_json::from_value(body).unwrap();
        assert!(req.service_type.is_none());
        assert!(req.service_protocol.is_none());
        assert!(share::auth_data_has_share_token(&req.auth_data));
    }

    #[test]
    fn gateway_connection_node_ips_serialization() {
        let base = GatewayConnection {
            connection_uuid: uuid::Uuid::nil(),
            node_id: uuid::Uuid::nil(),
            country_code: "FI".to_string(),
            country_name: "FI".to_string(),
            service_protocol: "awg".to_string(),
            env: "ru".to_string(),
            connection_label: "Moscow · AmneziaWG".to_string(),
            node_ips: None,
            node_ports: None,
            pool: None,
            pool_translation: None,
            weight: DEFAULT_NODE_WEIGHT,
            node_weights: None,
            config_rev: None,
        };
        // Single-address node without a pool: both optional fields are omitted
        // entirely (old shape, unchanged for old clients/nodes).
        let v = serde_json::to_value(&base).unwrap();
        assert!(v.get("node_ips").is_none());
        assert!(v.get("pool").is_none());
        assert!(v.get("pool_translation").is_none());
        // The legacy field is always present: without a numeric `weight` old
        // clients drop the server. Default is 100, and `node_weights` is not
        // needed for a single address.
        assert_eq!(v["weight"], serde_json::json!(100));
        assert!(v.get("node_weights").is_none());

        let mut weights = std::collections::BTreeMap::new();
        weights.insert("78.17.28.66".to_string(), 75);
        weights.insert("78.17.28.67".to_string(), 25);
        let weighted = GatewayConnection {
            weight: 75,
            node_weights: Some(weights),
            ..base.clone()
        };
        let v = serde_json::to_value(&weighted).unwrap();
        assert_eq!(v["weight"], serde_json::json!(75));
        assert_eq!(v["node_weights"]["78.17.28.66"], serde_json::json!(75));
        assert_eq!(v["node_weights"]["78.17.28.67"], serde_json::json!(25));

        let multi = GatewayConnection {
            node_ips: Some(vec!["78.17.28.66".to_string(), "78.17.28.67".to_string()]),
            ..base
        };
        let v = serde_json::to_value(&multi).unwrap();
        assert_eq!(
            v["node_ips"],
            serde_json::json!(["78.17.28.66", "78.17.28.67"])
        );
        assert!(v.get("config_rev").is_none());
    }

    fn node_with_ips(uuid: u128, address: &str, ips: &[&str], port: u16) -> fcore::Node {
        let mut inbound = wg_inbound();
        inbound.port = port;
        if let Some(wg) = inbound.wg.as_mut() {
            wg.port = port;
        }
        fcore::Node {
            uuid: uuid::Uuid::from_u128(uuid),
            env: Env::Ru,
            hostname: format!("n{uuid}"),
            address: address.parse().unwrap(),
            status: fcore::NodeStatus::Online,
            label: "n".into(),
            interface: "eth0".into(),
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
            inbounds: [(Tag::Wireguard, inbound)].into(),
            cores: 1,
            max_bandwidth_bps: 1,
            country: "RU".into(),
            r#type: fcore::NodeType::Node,
            cluster: None,
            node_ips: if ips.is_empty() {
                None
            } else {
                Some(ips.iter().map(|ip| ip.parse().unwrap()).collect())
            },
            pool: Some("Russia".into()),
            pool_translation: None,
            node_weights: BTreeMap::new(),
        }
    }

    /// Сэмпл контракта для клиента: ровно те байты, которые уезжают в JSON.
    ///
    /// Данные — реальный состав EU-пула (адреса и порты из прод-базы на
    /// 2026-10-30), протокол для примера WireGuard: форма карточки от протокола
    /// не зависит. Запуск с печатью:
    ///
    /// ```text
    /// cargo test --bin api contract_sample -- --nocapture
    /// ```
    #[test]
    fn contract_sample_pool_card() {
        // EU Рилзопровод как есть: 6 нод, 10 адресов, у всех 443.
        let eu = [
            node_with_ips(1, "177.177.20.172", &[], 443),                        // at
            node_with_ips(2, "93.89.220.48", &["93.89.220.48", "93.89.220.49"], 443), // ee
            node_with_ips(
                3,
                "185.156.108.232",
                &["185.156.108.232", "45.137.42.16"],
                443,
            ),                                                                    // eu01
            node_with_ips(4, "138.124.113.88", &[], 443),                         // eu02
            node_with_ips(
                5,
                "109.120.178.116",
                &["109.120.178.116", "77.221.152.88"],
                443,
            ),                                                                    // fr
            node_with_ips(
                6,
                "195.133.80.155",
                &["195.133.80.155", "195.133.80.156"],
                443,
            ),                                                                    // pl
        ];
        let members: Vec<&fcore::Node> = eu.iter().collect();
        let ports = node_ports_of_members(&members, Tag::Wireguard);

        let card = GatewayConnection {
            connection_uuid: uuid::Uuid::from_u128(0xc0ffee),
            node_id: eu[0].uuid,
            country_code: "EU".into(),
            country_name: "EU".into(),
            service_protocol: "wireguard".into(),
            env: "production".into(),
            connection_label: "EU Рилзопровод · WireGuard".into(),
            node_ips: Some(ports.keys().cloned().collect()),
            node_ports: Some(ports.clone()),
            pool: Some("EU Рилзопровод".into()),
            pool_translation: None,
            weight: DEFAULT_NODE_WEIGHT,
            node_weights: None,
            config_rev: Some(config_rev_pool(&members, Tag::Wireguard, &ports)),
        };
        let json = serde_json::to_string_pretty(&card).unwrap();
        println!("\n--- SAMPLE: EU pool, все порты 443 ---\n{json}\n");

        // Форма, которую читает клиент.
        assert_eq!(ports.len(), 10, "10 адресов в EU-пуле");
        assert!(ports.values().all(|port| *port == 443));

        // Второй сэмпл: порты у членов пула разные — то, ради чего поле нужно.
        let a = node_with_ips(11, "93.89.220.48", &["93.89.220.48", "93.89.220.49"], 443);
        let b = node_with_ips(12, "77.221.152.88", &[], 8443);
        let c = node_with_ips(13, "45.146.91.122", &[], 51820);
        let mixed_members = [&a, &b, &c];
        let mixed = node_ports_of_members(&mixed_members, Tag::Wireguard);
        let mixed_card = GatewayConnection {
            node_ips: Some(mixed.keys().cloned().collect()),
            node_ports: Some(mixed.clone()),
            config_rev: Some(config_rev_pool(&mixed_members, Tag::Wireguard, &mixed)),
            ..card
        };
        let json = serde_json::to_string_pretty(&mixed_card).unwrap();
        println!("\n--- SAMPLE: пул с разными портами ---\n{json}\n");

        assert_eq!(
            mixed,
            BTreeMap::from([
                ("93.89.220.48".to_string(), 443u16),
                ("93.89.220.49".to_string(), 443u16),
                ("77.221.152.88".to_string(), 8443u16),
                ("45.146.91.122".to_string(), 51820u16),
            ])
        );
    }

    /// Pool weights: one entry per address (not per node), default 100, equal
    /// values are fine.
    #[test]
    fn pool_node_weights_are_per_address_with_default_100() {
        let mut a = node_with_ips(21, "93.89.220.48", &["93.89.220.48", "93.89.220.49"], 443);
        a.node_weights = BTreeMap::from([
            ("93.89.220.48".to_string(), 75),
            ("93.89.220.49".to_string(), 25),
        ]);
        // The second node sets no weight at all — its address weighs 100.
        let b = node_with_ips(22, "77.221.152.88", &[], 443);
        let members = [&a, &b];

        let ports = node_ports_of_members(&members, Tag::Wireguard);
        let weights = node_weights_of_ips(&members, &ports);

        assert_eq!(
            weights,
            BTreeMap::from([
                ("93.89.220.48".to_string(), 75),
                ("93.89.220.49".to_string(), 25),
                ("77.221.152.88".to_string(), 100),
            ]),
            "each address has its own weight; unset means 100"
        );
        assert_eq!(
            weights.keys().collect::<Vec<_>>(),
            ports.keys().collect::<Vec<_>>(),
            "weight keys and port keys must match"
        );
        // Legacy field for old clients = the highest address weight.
        assert_eq!(weights.values().copied().max(), Some(100));
    }

    /// Default 100, repeated weights, and zero treated as "not set".
    #[test]
    fn node_weight_defaults_duplicates_and_zero() {
        let mut node = node_with_ips(31, "1.2.3.4", &["1.2.3.4", "5.6.7.8"], 443);
        assert_eq!(node.weight_for("1.2.3.4"), 100);
        assert_eq!(node.weight_for("5.6.7.8"), 100);
        assert_eq!(node.max_weight(), 100);

        // Equal weights on neighbour addresses are not an error.
        node.node_weights =
            BTreeMap::from([("1.2.3.4".to_string(), 50), ("5.6.7.8".to_string(), 50)]);
        assert_eq!(node.weights_by_ip().len(), 2);
        assert!(node.weights_by_ip().values().all(|w| *w == 50));
        assert_eq!(node.max_weight(), 50);

        // A node running an old build sends 0 instead of omitting the field.
        node.node_weights = BTreeMap::from([("1.2.3.4".to_string(), 0)]);
        assert_eq!(node.weight_for("1.2.3.4"), 100);
        assert_eq!(node.weight_for("5.6.7.8"), 100);
    }

    /// Полный ответ `/v1/services` в том виде, в котором он уезжает клиенту:
    /// обёртка сервиса + карточки коннектов. Печать:
    ///
    /// ```text
    /// cargo test --bin api contract_sample_services_response -- --nocapture
    /// ```
    #[test]
    fn contract_sample_services_response() {
        let a = node_with_ips(1, "93.89.220.48", &["93.89.220.48", "93.89.220.49"], 443);
        let b = node_with_ips(2, "77.221.152.88", &[], 8443);
        let members = [&a, &b];
        let ports = node_ports_of_members(&members, Tag::Wireguard);

        let pool_card = GatewayConnection {
            connection_uuid: uuid::Uuid::from_u128(0xc0ffee),
            node_id: a.uuid,
            country_code: "RU".into(),
            country_name: "Russia".into(),
            service_protocol: "awg-mobile".into(),
            env: "production".into(),
            connection_label: "RU Рилзопровод · AmneziaWG".into(),
            node_ips: Some(ports.keys().cloned().collect()),
            node_ports: Some(ports.clone()),
            pool: Some("RU Рилзопровод".into()),
            pool_translation: None,
            weight: DEFAULT_NODE_WEIGHT,
            node_weights: None,
            config_rev: Some(config_rev_pool(&members, Tag::Wireguard, &ports)),
        };

        // Одиночная нода: карты портов нет.
        let single_card = GatewayConnection {
            connection_uuid: uuid::Uuid::from_u128(0xbeef),
            node_id: b.uuid,
            country_code: "NL".into(),
            country_name: "Netherlands".into(),
            service_protocol: "awg-mobile".into(),
            env: "production".into(),
            connection_label: "NL777 · AmneziaWG".into(),
            node_ips: None,
            node_ports: None,
            pool: None,
            pool_translation: None,
            weight: DEFAULT_NODE_WEIGHT,
            node_weights: None,
            config_rev: None,
        };

        let response = GatewayServicesResponse {
            user_country_code: None,
            services: vec![GatewayService {
                service_type: "amnezia-premium".into(),
                service_protocol: "vless".into(),
                service_info: GatewayServiceInfo {
                    name: "Premium".into(),
                    price: "299".into(),
                    speed: "unlimited".into(),
                    timelimit: "30".into(),
                    region: "worldwide".into(),
                },
                service_description: GatewayServiceDescription {
                    description: "FRKN Premium".into(),
                    card_description: "Premium access".into(),
                    features: "all protocols".into(),
                },
                available_countries: vec![
                    GatewayCountry {
                        country_code: "RU".into(),
                        country_name: "Russia".into(),
                        connection_uuid: Some(pool_card.connection_uuid),
                        connection_label: "RU Рилзопровод · AmneziaWG".into(),
                        pool: Some("RU Рилзопровод".into()),
                        pool_translation: None,
                    },
                    GatewayCountry {
                        country_code: "NL".into(),
                        country_name: "Netherlands".into(),
                        connection_uuid: Some(single_card.connection_uuid),
                        connection_label: "NL777 · AmneziaWG".into(),
                        pool: None,
                        pool_translation: None,
                    },
                ],
                connections: vec![pool_card, single_card],
                store_endpoint: "https://frkn.org".into(),
                is_available: true,
                subscription: GatewaySubscriptionMeta {
                    end_date: Some("2026-12-01".into()),
                },
            }],
        };

        let json = serde_json::to_string_pretty(&response).unwrap();
        println!("\n--- RESPONSE /v1/services ---\n{json}\n");

        // Контракт: у пула карта есть, у одиночной ноды нет.
        let v = serde_json::to_value(&response).unwrap();
        let conns = &v["services"][0]["connections"];
        assert_eq!(
            conns[0]["node_ports"],
            serde_json::json!({
                "93.89.220.48": 443,
                "93.89.220.49": 443,
                "77.221.152.88": 8443
            })
        );
        assert!(conns[1].get("node_ports").is_none());
        assert!(conns[1].get("node_ips").is_none());
    }

    /// Внутри ноды несколько адресов — один интерфейс, значит один порт.
    #[test]
    fn one_node_keeps_a_single_port_across_its_ips() {
        let node = node_with_ips(1, "93.89.220.48", &["93.89.220.48", "93.89.220.49"], 443);
        let ports = node_ports_of_members(&[&node], Tag::Wireguard);
        assert_eq!(
            ports,
            BTreeMap::from([
                ("93.89.220.48".to_string(), 443u16),
                ("93.89.220.49".to_string(), 443u16),
            ])
        );
    }

    /// Главное требование: в пуле порты у нод разные, и клиент получает карту
    /// адрес → порт, а не один порт на весь пул.
    #[test]
    fn pool_members_may_listen_on_different_ports() {
        let a = node_with_ips(1, "93.89.220.48", &["93.89.220.48", "93.89.220.49"], 443);
        let b = node_with_ips(2, "77.221.152.88", &[], 8443);
        let ports = node_ports_of_members(&[&a, &b], Tag::Wireguard);

        assert_eq!(
            ports,
            BTreeMap::from([
                ("93.89.220.48".to_string(), 443u16),
                ("93.89.220.49".to_string(), 443u16),
                ("77.221.152.88".to_string(), 8443u16),
            ])
        );
        // node_ips — те же ключи карты, один источник правды
        let ips: Vec<&String> = ports.keys().collect();
        assert_eq!(ips.len(), 3);
    }

    /// Нода без инбаунда этого протокола не попадает ни в карту, ни в node_ips:
    /// адрес, на котором никто не слушает, клиенту бесполезен.
    #[test]
    fn member_without_the_inbound_is_skipped() {
        let a = node_with_ips(1, "93.89.220.48", &[], 443);
        let mut b = node_with_ips(2, "77.221.152.88", &[], 8443);
        b.inbounds.clear();

        let ports = node_ports_of_members(&[&a, &b], Tag::Wireguard);
        assert_eq!(ports.len(), 1);
        assert!(ports.contains_key("93.89.220.48"));

        // и для протокола, которого нет ни у кого, — пусто, а не мусор
        assert!(node_ports_of_members(&[&a, &b], Tag::Hysteria2).is_empty());
    }

    /// Версия карточки пула обязана меняться от смены порта: иначе клиент
    /// останется на старом адресе.
    #[test]
    fn pool_config_rev_tracks_ports() {
        let a = node_with_ips(1, "93.89.220.48", &[], 443);
        let b = node_with_ips(2, "77.221.152.88", &[], 8443);
        let members = [&a, &b];
        let before = node_ports_of_members(&members, Tag::Wireguard);

        let mut b2 = b.clone();
        b2.inbounds.get_mut(&Tag::Wireguard).unwrap().port = 51820;
        let members2 = [&a, &b2];
        let after = node_ports_of_members(&members2, Tag::Wireguard);

        assert_ne!(
            config_rev_pool(&members, Tag::Wireguard, &before),
            config_rev_pool(&members2, Tag::Wireguard, &after)
        );
        // стабильность: тот же вход — та же версия
        assert_eq!(
            config_rev_pool(&members, Tag::Wireguard, &before),
            config_rev_pool(&members, Tag::Wireguard, &before.clone())
        );
    }

    /// Формат, который читает клиент: `node_ports` — объект «адрес → порт»
    /// (`servers_model.cpp::portForNodeIp`).
    #[test]
    fn node_ports_serialize_as_an_object_next_to_node_ips() {
        let conn = GatewayConnection {
            connection_uuid: uuid::Uuid::nil(),
            node_id: uuid::Uuid::nil(),
            country_code: "RU".into(),
            country_name: "RU".into(),
            service_protocol: "awg-mobile".into(),
            env: "ru".into(),
            connection_label: "Russia · AmneziaWG".into(),
            node_ips: Some(vec!["93.89.220.48".into(), "77.221.152.88".into()]),
            node_ports: Some(BTreeMap::from([
                ("93.89.220.48".to_string(), 443u16),
                ("77.221.152.88".to_string(), 8443u16),
            ])),
            pool: Some("Russia".into()),
            pool_translation: None,
            weight: DEFAULT_NODE_WEIGHT,
            node_weights: None,
            config_rev: Some("abc".into()),
        };
        let v = serde_json::to_value(&conn).unwrap();
        assert_eq!(
            v["node_ports"],
            serde_json::json!({"77.221.152.88": 8443, "93.89.220.48": 443})
        );
        // старый клиент читает node_ips как раньше
        assert_eq!(
            v["node_ips"],
            serde_json::json!(["93.89.220.48", "77.221.152.88"])
        );
        // и не видит ключа, которого не знает
        assert!(v.get("endpoints").is_none(), "лишних полей быть не должно");
    }

    /// Одиночная нода: порт один и он уже в конфиге, карта была бы дублированием.
    #[test]
    fn single_node_omits_node_ports() {
        let conn = GatewayConnection {
            connection_uuid: uuid::Uuid::nil(),
            node_id: uuid::Uuid::nil(),
            country_code: "RU".into(),
            country_name: "RU".into(),
            service_protocol: "awg-mobile".into(),
            env: "ru".into(),
            connection_label: "Moscow · AmneziaWG".into(),
            node_ips: None,
            node_ports: None,
            pool: None,
            pool_translation: None,
            weight: DEFAULT_NODE_WEIGHT,
            node_weights: None,
            config_rev: None,
        };
        let v = serde_json::to_value(&conn).unwrap();
        for field in ["node_ips", "node_ports", "pool", "config_rev"] {
            assert!(v.get(field).is_none(), "{field} must be omitted");
        }
    }

    /// `config_rev` пула должен реагировать на смену серверного материала, а не
    /// только адресов. Иначе смена ключа или obfuscation не заставит клиента
    /// перечитать конфиг.
    #[test]
    fn pool_config_rev_tracks_server_material() {
        let a = node_with_ips(1, "93.89.220.48", &[], 443);
        let b = node_with_ips(2, "77.221.152.88", &[], 8443);
        let members = [&a, &b];
        let ports = node_ports_of_members(&members, Tag::Wireguard);
        let before = config_rev_pool(&members, Tag::Wireguard, &ports);

        // тот же состав и адреса, но другой ключ на второй ноде
        let mut b2 = b.clone();
        if let Some(wg) = b2.inbounds.get_mut(&Tag::Wireguard).and_then(|i| i.wg.as_mut()) {
            wg.keys = WgKeys {
                privkey: WgKeys::default().privkey,
            };
        }
        let ports2 = node_ports_of_members(&[&a, &b2], Tag::Wireguard);
        assert_eq!(ports, ports2, "адреса не менялись");
        let after = config_rev_pool(&[&a, &b2], Tag::Wireguard, &ports2);
        assert_ne!(before, after, "смена ключа обязана поднять версию");
    }

    #[test]
    fn config_rev_tracks_server_material_only() {
        let inbound = wg_inbound();
        let node = fcore::Node {
            uuid: uuid::Uuid::nil(),
            env: Env::Ru,
            hostname: "uncle".into(),
            address: "203.0.113.7".parse().unwrap(),
            status: fcore::NodeStatus::Online,
            label: "Uncle Sam".into(),
            interface: "eth0".into(),
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
            inbounds: [(Tag::Wireguard, inbound)].into(),
            cores: 1,
            max_bandwidth_bps: 1,
            country: "US".into(),
            r#type: fcore::NodeType::Node,
            cluster: None,
            node_ips: None,
            pool: None,
            pool_translation: None,
            node_weights: BTreeMap::new(),
        };
        let rev = config_rev(&node, Tag::Wireguard, &[]).unwrap();
        assert_eq!(
            config_rev(&node, Tag::Wireguard, &[]).as_deref(),
            Some(rev.as_str())
        );
        assert_eq!(rev.len(), 64);

        let mut renamed = node.clone();
        renamed.label = "Other".into();
        renamed.country = "DE".into();
        assert_eq!(
            config_rev(&renamed, Tag::Wireguard, &[]).as_deref(),
            Some(rev.as_str())
        );

        let mut moved = node.clone();
        moved.address = "203.0.113.8".parse().unwrap();
        assert_ne!(config_rev(&moved, Tag::Wireguard, &[]).unwrap(), rev);

        let with_ips = config_rev(
            &node,
            Tag::Wireguard,
            &["203.0.113.7".into(), "203.0.113.9".into()],
        )
        .unwrap();
        assert_ne!(with_ips, rev);
        assert!(config_rev(&node, Tag::Hysteria2, &[]).is_none());
    }

    #[test]
    fn gateway_connection_pool_serialization() {
        let base = GatewayConnection {
            connection_uuid: uuid::Uuid::nil(),
            node_id: uuid::Uuid::nil(),
            country_code: "FI".to_string(),
            country_name: "FI".to_string(),
            service_protocol: "awg".to_string(),
            env: "ru".to_string(),
            connection_label: "Finland · AmneziaWG".to_string(),
            node_ips: None,
            node_ports: None,
            pool: Some("Finland".to_string()),
            pool_translation: Some(
                [
                    ("en".to_string(), "Finland".to_string()),
                    ("ru".to_string(), "Финляндия".to_string()),
                    ("uk".to_string(), "Фінляндія".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            weight: DEFAULT_NODE_WEIGHT,
            node_weights: None,
            config_rev: None,
        };
        let v = serde_json::to_value(&base).unwrap();
        assert_eq!(v["pool"], serde_json::json!("Finland"));
        assert_eq!(v["pool_translation"]["ru"], serde_json::json!("Финляндия"));
        assert_eq!(v["pool_translation"]["uk"], serde_json::json!("Фінляндія"));
    }

    #[test]
    fn share_config_response_carries_display_fields() {
        // The share recipient has no /v1/services access: the config response
        // must carry share_label/country/service_protocol/service_type.
        let resp = GatewayConfigResponse {
            config: "Y2Zn".to_string(),
            supported_protocols: vec!["awg".to_string()],
            service_info: serde_json::json!({"name": "Moscow", "type": share::SHARE_SERVICE_TYPE}),
            api_config: serde_json::json!({
                "service_type": share::SHARE_SERVICE_TYPE,
                "service_protocol": "awg",
                "user_country_code": "",
                "server_country_code": null
            }),
            share_label: Some("Android Mama".to_string()),
            country_code: Some("FI".to_string()),
            country_name: Some("FI".to_string()),
            service_protocol: Some("awg".to_string()),
            service_type: Some(share::SHARE_SERVICE_TYPE.to_string()),
            node_ips: Some(vec!["78.17.28.66".to_string(), "78.17.28.67".to_string()]),
            node_ports: Some(BTreeMap::from([(
                "78.17.28.66".to_string(),
                443u16,
            )])),
            config_rev: Some("abc".to_string()),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["share_label"], "Android Mama");
        assert_eq!(v["country_code"], "FI");
        assert_eq!(v["country_name"], "FI");
        assert_eq!(v["service_protocol"], "awg");
        assert_eq!(v["service_type"], "amnezia-premium");
        assert_eq!(v["api_config"]["service_type"], "amnezia-premium");
        assert_eq!(
            v["node_ips"],
            serde_json::json!(["78.17.28.66", "78.17.28.67"])
        );
        assert_eq!(v["config_rev"], "abc");

        // Owner branch: display fields are omitted entirely.
        let owner = GatewayConfigResponse {
            config: resp.config.clone(),
            supported_protocols: resp.supported_protocols.clone(),
            service_info: resp.service_info.clone(),
            api_config: resp.api_config.clone(),
            share_label: None,
            country_code: None,
            country_name: None,
            service_protocol: None,
            service_type: None,
            node_ips: None,
            node_ports: None,
            config_rev: None,
        };
        let v = serde_json::to_value(&owner).unwrap();
        for field in [
            "share_label",
            "country_code",
            "country_name",
            "service_protocol",
            "service_type",
            "node_ips",
            "node_ports",
            "config_rev",
        ] {
            assert!(v.get(field).is_none(), "{} must be omitted", field);
        }
    }

    #[test]
    fn private_connection_title_uses_label_not_host() {
        assert_eq!(
            connection_title("Suomi", "fi.example", "203.0.113.7", "FI", false, None),
            "Suomi"
        );
        assert_eq!(
            connection_title(
                "vps.example",
                "vps.example",
                "203.0.113.7",
                "FI",
                true,
                None
            ),
            "FI| Private"
        );
        assert_eq!(
            connection_title(
                "vps.example",
                "vps.example",
                "203.0.113.7",
                "FI",
                false,
                None
            ),
            "vps.example"
        );
        assert_eq!(
            connection_title(
                "CZ| Private",
                "vps.example",
                "203.0.113.7",
                "XX",
                true,
                None
            ),
            "CZ| Private"
        );
        assert_eq!(
            connection_title(
                "203.0.113.7",
                "vps.example",
                "203.0.113.7",
                "FI",
                true,
                Some("Кухня")
            ),
            "Кухня"
        );
        // A named device prefixes the node it lives on: with an env-wide device
        // the same name would otherwise repeat on every server.
        assert_eq!(
            connection_title(
                "Kitchen",
                "vps.example",
                "203.0.113.7",
                "FI",
                true,
                Some("Кухня")
            ),
            "Кухня (Kitchen)"
        );
        assert_eq!(
            connection_title(
                "Premium-a65e52c6af0e",
                "host",
                "203.0.113.7",
                "CZ",
                false,
                Some("Телефон")
            ),
            "Телефон (Premium-a65e52c6af0e)"
        );
        assert_eq!(
            connection_title(
                "NL4",
                "nl4.example",
                "203.0.113.7",
                "NL",
                false,
                Some("Мама")
            ),
            "Мама (NL4)"
        );
        assert_eq!(
            connection_title("", "vps.example", "203.0.113.7", "nl", false, None),
            "NL"
        );
    }

    #[test]
    fn pool_collapses_to_one_card_and_drops_offline_ips() {
        use fcore::{Connection, NodeStatus, NodeType, Proto};
        use std::collections::HashMap;

        fn inbound() -> fcore::Inbound {
            fcore::Inbound {
                tag: Tag::VlessTcpReality,
                port: 443,
                stream_settings: None,
                wg: None,
                awg: None,
                h2: None,
                mtproto_secret: None,
            }
        }
        fn node(
            id: &str,
            addr: &str,
            extra: Option<&str>,
            pool: Option<&str>,
            status: NodeStatus,
        ) -> fcore::Node {
            let mut ips = vec![addr.parse().unwrap()];
            if let Some(extra) = extra {
                ips.push(extra.parse().unwrap());
            }
            fcore::Node {
                uuid: uuid::Uuid::parse_str(id).unwrap(),
                env: Env::Ru,
                hostname: addr.into(),
                address: addr.parse().unwrap(),
                status,
                label: "PL".into(),
                interface: "eth0".into(),
                created_at: chrono::Utc::now(),
                modified_at: chrono::Utc::now(),
                inbounds: [(Tag::VlessTcpReality, inbound())].into_iter().collect(),
                cores: 1,
                max_bandwidth_bps: 1,
                country: "PL".into(),
                r#type: NodeType::Node,
                cluster: None,
                node_ips: Some(ips),
                pool: pool.map(|s| s.to_string()),
                pool_translation: None,
                node_weights: BTreeMap::new(),
            }
        }

        let pl = node(
            "11111111-1111-1111-1111-111111111111",
            "1.1.1.1",
            Some("1.1.1.2"),
            Some("EU Рилзопровод"),
            NodeStatus::Online,
        );
        let fi = node(
            "22222222-2222-2222-2222-222222222222",
            "2.2.2.1",
            Some("2.2.2.2"),
            Some("EU Рилзопровод"),
            NodeStatus::Online,
        );
        let mut nodes: HashMap<Env, Vec<fcore::Node>> = HashMap::new();
        nodes.insert(Env::Ru, vec![pl, fi]);
        let conn_id = uuid::Uuid::new_v4();
        let conn = Connection::new(&Env::Ru, None, Proto::new_xray(&Tag::VlessTcpReality), None);
        let conns = [(conn_id, conn)];
        let labels = HashMap::<uuid::Uuid, String>::new();
        let pins = HashMap::<uuid::Uuid, uuid::Uuid>::new();
        let real = |cards: Vec<GatewayConnection>| {
            cards
                .into_iter()
                .filter(|c| c.connection_uuid != uuid::Uuid::nil())
                .collect::<Vec<_>>()
        };

        let cards = real(connections_for_protocol(
            &nodes,
            &pins,
            &labels,
            "vless",
            Some(&conns),
            true,
        ));
        assert_eq!(cards.len(), 1);
        assert_eq!(
            cards[0].connection_label,
            "EU Рилзопровод · VLESS TCP Reality"
        );
        assert_eq!(
            cards[0].node_ips.as_deref(),
            Some(
                ["1.1.1.1", "1.1.1.2", "2.2.2.1", "2.2.2.2"]
                    .map(str::to_string)
                    .as_slice()
            )
        );

        nodes.get_mut(&Env::Ru).unwrap()[1].status = NodeStatus::Offline;
        let cards = real(connections_for_protocol(
            &nodes,
            &pins,
            &labels,
            "vless",
            Some(&conns),
            true,
        ));
        assert_eq!(cards.len(), 1);
        assert_eq!(
            cards[0].node_ips.as_deref(),
            Some(["1.1.1.1", "1.1.1.2"].map(str::to_string).as_slice())
        );

        for n in nodes.get_mut(&Env::Ru).unwrap() {
            n.pool = None;
            n.status = NodeStatus::Online;
        }
        let cards = real(connections_for_protocol(
            &nodes,
            &pins,
            &labels,
            "vless",
            Some(&conns),
            true,
        ));
        assert_eq!(cards.len(), 2);

        for n in nodes.get_mut(&Env::Ru).unwrap() {
            n.pool = Some("EU Рилзопровод".into());
        }
        let cards = real(connections_for_protocol(
            &nodes,
            &pins,
            &labels,
            "vless",
            Some(&conns),
            false,
        ));
        assert_eq!(cards.len(), 2);
    }
}

#[cfg(test)]
mod expiry_tests {
    use super::subscription_expired_response;

    #[tokio::test]
    async fn expired_response_is_402_with_structured_body() {
        let resp = subscription_expired_response(None);
        assert_eq!(resp.status(), warp::http::StatusCode::PAYMENT_REQUIRED);

        let body = warp::hyper::body::to_bytes(resp.into_body()).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "subscription_expired");
        assert_eq!(json["end_date"], "");
        assert!(json["message"].as_str().unwrap().contains("Renew"));
    }

    #[tokio::test]
    async fn expired_response_includes_end_date() {
        let ts = chrono::Utc::now();
        let resp = subscription_expired_response(Some(ts));
        let body = warp::hyper::body::to_bytes(resp.into_body()).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["end_date"], ts.to_rfc3339());
        assert!(json["message"].as_str().unwrap().contains(&ts.to_rfc3339()));
    }
}

#[cfg(test)]
mod mobile_proto_tests {
    use super::{proto_label, proto_matches};
    use fcore::Tag;

    #[test]
    fn awg_mobile_is_a_distinct_protocol() {
        assert!(proto_matches(Tag::AmneziaWgMobile, "awg-mobile"));
        assert!(!proto_matches(Tag::AmneziaWgMobile, "awg"));
        assert!(proto_matches(Tag::AmneziaWg, "awg"));
        assert!(!proto_matches(Tag::AmneziaWg, "awg-mobile"));
        assert_eq!(proto_label(Tag::AmneziaWgMobile), "awg-mobile");
        assert_eq!(proto_label(Tag::AmneziaWg), "awg");
    }
}


// ============================================================================
// NODE LOAD (`POST /v1/nodes/load`)
// ============================================================================

/// Upper bound of `node_ids` accepted in a single request.
pub const MAX_LOAD_NODES: usize = 50;
/// Rolling window the reported load is averaged over.
pub const LOAD_WINDOW_MS: i64 = 60_000;

#[derive(Debug, Deserialize)]
pub struct GatewayNodesLoadRequest {
    #[serde(rename = "auth_data")]
    pub auth_data: serde_json::Value,
    #[serde(rename = "node_ids", default)]
    pub node_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct GatewayNodesLoadResponse {
    /// Moment the figures were computed (RFC3339) so the client can age them out.
    pub at: String,
    /// node_id -> 0..1. A node without fresh metrics is absent on purpose.
    pub load: BTreeMap<String, f64>,
}

/// Collapses the raw utilizations into the 0..1 score the client multiplies
/// its weight by. Missing terms are skipped: a node that reports no CPU still
/// gets a bandwidth-based score. `None` means "no data at all" and the caller
/// must omit the node — a `0.0` would read as "free" and pull every connection
/// onto it.
pub fn node_load_score(
    bandwidth_util: Option<f64>,
    conn_util: Option<f64>,
    cpu_util: Option<f64>,
    threshold: f64,
) -> Option<f64> {
    let mixed = match (conn_util, cpu_util) {
        (Some(c), Some(u)) => Some(0.6 * c + 0.4 * u),
        (Some(c), None) => Some(c),
        (None, Some(u)) => Some(u),
        (None, None) => None,
    };

    let score = match (bandwidth_util, mixed) {
        (Some(b), Some(m)) => b.max(m),
        (Some(b), None) => b,
        (None, Some(m)) => m,
        (None, None) => return None,
    }
    .clamp(0.0, 1.0);

    Some(if score >= threshold { 1.0 } else { score })
}

/// Parses, deduplicates and caps `node_ids`. Unparsable ids are dropped
/// instead of failing the whole request: one broken id must not cost the
/// client the load data of the whole pool.
pub fn parse_node_ids(raw: &[String]) -> Vec<uuid::Uuid> {
    let mut out: Vec<uuid::Uuid> = Vec::with_capacity(raw.len().min(MAX_LOAD_NODES));
    for id in raw {
        let Ok(id) = uuid::Uuid::parse_str(id.trim()) else {
            continue;
        };
        if out.contains(&id) {
            continue;
        }
        out.push(id);
        if out.len() == MAX_LOAD_NODES {
            break;
        }
    }
    out
}

fn load_rate_limited() -> warp::reply::Response {
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({
            "status": 429,
            "message": "node load polled too often",
        })),
        warp::http::StatusCode::TOO_MANY_REQUESTS,
    )
    .into_response()
}

/// Share of the node's declared bandwidth used in the window (rx + tx).
///
/// Only the node's default (uplink) interface counts: `awg0`/`wg0`/`lo` carry
/// the same bytes again, so summing every interface would report double load.
/// If the configured name matches nothing (kernel rename), the busiest
/// interface per direction is used instead of the sum.
fn node_bandwidth_util(
    metrics: &MetricStorage,
    node_id: &uuid::Uuid,
    default_interface: &str,
    max_bandwidth_bps: i64,
) -> Option<f64> {
    if max_bandwidth_bps <= 0 {
        return None;
    }

    let rx_name = format!("net.{default_interface}.rx_bps");
    let tx_name = format!("net.{default_interface}.tx_bps");

    let rx = metrics.node_series_avg_window(node_id, |name| name == rx_name, LOAD_WINDOW_MS);
    let tx = metrics.node_series_avg_window(node_id, |name| name == tx_name, LOAD_WINDOW_MS);

    let (rx, tx) = if rx.is_empty() && tx.is_empty() {
        let peak = |values: Vec<f64>| values.into_iter().fold(f64::NEG_INFINITY, f64::max);
        (
            peak(metrics.node_series_avg_window(
                node_id,
                |name| name.starts_with("net.") && name.ends_with(".rx_bps"),
                LOAD_WINDOW_MS,
            )),
            peak(metrics.node_series_avg_window(
                node_id,
                |name| name.starts_with("net.") && name.ends_with(".tx_bps"),
                LOAD_WINDOW_MS,
            )),
        )
    } else {
        (rx.iter().sum::<f64>(), tx.iter().sum::<f64>())
    };

    let bps = match (rx.is_finite(), tx.is_finite()) {
        (true, true) => rx + tx,
        (true, false) => rx,
        (false, true) => tx,
        (false, false) => return None,
    };

    Some((bps / max_bandwidth_bps as f64).clamp(0.0, 1.0))
}

/// Active peers of the node, or `None` when it publishes no connection
/// counters at all (old fnode, or a protocol without per-peer accounting).
fn node_active_peers(metrics: &MetricStorage, node_id: &uuid::Uuid) -> Option<f64> {
    if let Some(peers) = metrics.node_online_peers(node_id) {
        return Some(peers);
    }

    // xray inbounds count their own connections; used when a node serves no
    // wg/awg at all.
    let series = metrics.node_series_avg_window(
        node_id,
        |name| name.starts_with("net.inbound.") && name.ends_with(".connections"),
        LOAD_WINDOW_MS,
    );
    if series.is_empty() {
        return None;
    }

    Some(series.iter().sum())
}

/// Median, used as the stand-in value for nodes that report no counters.
/// Robust against one huge node skewing the fleet-wide figure.
fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    })
}

/// Share of the fleet-wide connection capacity taken by this node.
fn node_conn_util(peers: f64, capacity: u32) -> Option<f64> {
    if capacity == 0 {
        return None;
    }
    Some((peers / capacity as f64).clamp(0.0, 1.0))
}

/// Average CPU utilization of the node, 0..1.
fn node_cpu_util(metrics: &MetricStorage, node_id: &uuid::Uuid) -> Option<f64> {
    let series = metrics.node_series_avg_window(
        node_id,
        |name| name.starts_with("sys.cpu."),
        LOAD_WINDOW_MS,
    );
    if series.is_empty() {
        return None;
    }

    let avg = series.iter().sum::<f64>() / series.len() as f64;
    Some((avg / 100.0).clamp(0.0, 1.0))
}

/// `POST /v1/nodes/load` — current load of the requested nodes.
///
/// Separate from `/v1/config` on purpose: a config is cached for hours, while
/// the load has to be current at the moment the client picks an address. The
/// answer is intentionally sparse — a missing node id means "unknown", never
/// "free".
pub async fn gateway_nodes_load_handler<N, C, S>(
    req: GatewayNodesLoadRequest,
    memory: MemSync<N, C, S>,
    metrics: std::sync::Arc<MetricStorage>,
    limiter: std::sync::Arc<crate::http::handlers::report::SubRateLimiter>,
    conn_capacity: u32,
    threshold: f64,
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
{
    let Some(sub_id) = extract_subscription_id(&req.auth_data) else {
        return Ok(http::bad_request("auth_data.id is required").into_response());
    };
    if !limiter.check(sub_id) {
        return Ok(load_rate_limited());
    }

    let ids = parse_node_ids(&req.node_ids);
    let mut load = BTreeMap::new();

    // First pass: what each node reports. Nodes without counters (old fnode)
    // must not be read as "zero" — they get the fleet median below.
    let mut nodes: Vec<(uuid::Uuid, String, i64, Option<f64>)> = Vec::new();
    {
        let mem = memory.memory.read().await;
        for id in ids {
            let Some(node) = mem.nodes.get_by_id(&id) else {
                continue;
            };
            let peers = node_active_peers(&metrics, &id);
            nodes.push((id, node.interface.clone(), node.max_bandwidth_bps, peers));
        }
    }

    let median_peers = median(nodes.iter().filter_map(|(_, _, _, p)| *p).collect());

    for (id, interface, max_bandwidth_bps, peers) in nodes {
        let bandwidth = node_bandwidth_util(&metrics, &id, &interface, max_bandwidth_bps);
        let conns = peers
            .or(median_peers)
            .and_then(|peers| node_conn_util(peers, conn_capacity));
        let cpu = node_cpu_util(&metrics, &id);

        if let Some(score) = node_load_score(bandwidth, conns, cpu, threshold) {
            load.insert(id.to_string(), (score * 100.0).round() / 100.0);
        }
    }

    Ok(warp::reply::json(&GatewayNodesLoadResponse {
        at: chrono::Utc::now().to_rfc3339(),
        load,
    })
    .into_response())
}

#[cfg(test)]
mod load_tests {
    use super::*;

    #[test]
    fn score_takes_the_worst_term() {
        // Bandwidth is the binding constraint: the mix must not water it down.
        let score = node_load_score(Some(0.9), Some(0.1), Some(0.1), 0.85).unwrap();
        assert_eq!(score, 1.0, "0.9 is above the threshold and must read as 1.0");

        let score = node_load_score(Some(0.5), Some(0.1), Some(0.1), 0.85).unwrap();
        assert!((score - 0.5).abs() < 1e-9);
    }

    #[test]
    fn score_mixes_connections_and_cpu() {
        let score = node_load_score(None, Some(0.5), Some(0.5), 0.85).unwrap();
        assert!((score - 0.5).abs() < 1e-9, "0.6*0.5 + 0.4*0.5 = 0.5, got {score}");

        // Without one of the terms the other is used as is, not diluted by zero.
        let score = node_load_score(None, Some(0.4), None, 0.85).unwrap();
        assert!((score - 0.4).abs() < 1e-9);
    }

    #[test]
    fn score_without_data_is_none_not_zero() {
        // A zero would mean "free" to the client and pull all traffic here.
        assert_eq!(node_load_score(None, None, None, 0.85), None);
    }

    #[test]
    fn score_is_clamped() {
        assert_eq!(node_load_score(Some(4.0), None, None, 0.85), Some(1.0));
        assert_eq!(node_load_score(Some(-1.0), None, None, 0.85), Some(0.0));
    }

    #[test]
    fn ids_are_deduplicated_capped_and_skipped_when_broken() {
        let mut raw: Vec<String> = vec!["not-a-uuid".to_string(), "   ".to_string()];
        for i in 0..60u32 {
            raw.push(format!("00000000-0000-0000-0000-{:012}", i));
        }
        raw.push("00000000-0000-0000-0000-000000000005".to_string());

        let ids = parse_node_ids(&raw);
        assert_eq!(ids.len(), MAX_LOAD_NODES);
        assert_eq!(ids[5].to_string(), "00000000-0000-0000-0000-000000000005");
        assert!(!ids.is_empty());
    }

    #[test]
    fn metrics_helpers_ignore_stale_series() {
        use fcore::metrics::MetricEnvelope;

        let storage = MetricStorage::new(100, 3600);
        let node_id = uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let mut tags = BTreeMap::new();
        tags.insert("hostname".to_string(), "test-node".to_string());

        // Fresh sample: 40% of the declared 100 Mbps.
        storage.insert_envelope(MetricEnvelope {
            node_id,
            name: "net.eth0.tx_bps".to_string(),
            value: 40_000_000.0,
            timestamp: now - 1_000,
            tags: tags.clone(),
        });
        // Stale sample from an hour ago must not be averaged in.
        storage.insert_envelope(MetricEnvelope {
            node_id,
            name: "net.eth0.rx_bps".to_string(),
            value: 900_000_000.0,
            timestamp: now - 3_600_000,
            tags: tags.clone(),
        });

        let util = node_bandwidth_util(&storage, &node_id, "eth0", 100_000_000).unwrap();
        assert!((util - 0.4).abs() < 1e-9, "only the fresh series counts, got {util}");

        // Tunnel traffic must not be counted twice: awg0 carries the same bytes
        // as eth0, so only the default interface is read.
        storage.insert_envelope(MetricEnvelope {
            node_id,
            name: "net.awg0.tx_bps".to_string(),
            value: 40_000_000.0,
            timestamp: now - 1_000,
            tags: BTreeMap::new(),
        });
        let util = node_bandwidth_util(&storage, &node_id, "eth0", 100_000_000).unwrap();
        assert!((util - 0.4).abs() < 1e-9, "awg0 must not be added, got {util}");

        storage.insert_envelope(MetricEnvelope {
            node_id,
            name: "net.inbound.amnezia-wg.connections".to_string(),
            value: 150.0,
            timestamp: now - 500,
            tags: BTreeMap::new(),
        });
        let peers = node_active_peers(&storage, &node_id).unwrap();
        assert!((peers - 150.0).abs() < 1e-9, "got {peers}");
        let util = node_conn_util(peers, 300).unwrap();
        assert!((util - 0.5).abs() < 1e-9, "got {util}");

        // Per-connection handshake state wins over the inbound counters: two of
        // four peers are online, so the node has 2 active connections.
        for (conn, online) in [("a", 1.0), ("b", 1.0), ("c", 0.0), ("d", 0.0)] {
            let mut conn_tags = BTreeMap::new();
            conn_tags.insert("conn_id".to_string(), conn.to_string());
            storage.insert_envelope(MetricEnvelope {
                node_id,
                name: "user.traffic.online".to_string(),
                value: online,
                timestamp: now - 400,
                tags: conn_tags,
            });
        }
        assert_eq!(storage.node_online_peers(&node_id), Some(2.0));

        // A connection that went away keeps its last sample forever; an hour-old
        // `1.0` must not count as a live peer.
        let mut dead_tags = BTreeMap::new();
        dead_tags.insert("conn_id".to_string(), "gone".to_string());
        storage.insert_envelope(MetricEnvelope {
            node_id,
            name: "user.traffic.online".to_string(),
            value: 1.0,
            timestamp: now - 3_600_000,
            tags: dead_tags,
        });
        assert_eq!(
            storage.node_online_peers(&node_id),
            Some(2.0),
            "stale series must not be counted"
        );

        // A node that reports nothing is not "free": it gets the fleet median.
        let silent = uuid::Uuid::parse_str("99999999-8888-7777-6666-555555555555").unwrap();
        assert_eq!(node_active_peers(&storage, &silent), None);
        assert_eq!(median(vec![2.0, 40.0, 8.0]), Some(8.0));
        assert_eq!(median(vec![10.0, 20.0]), Some(15.0));
        assert_eq!(median(Vec::new()), None);

        storage.insert_envelope(MetricEnvelope {
            node_id,
            name: "sys.cpu.0".to_string(),
            value: 25.0,
            timestamp: now - 500,
            tags: BTreeMap::new(),
        });
        let util = node_cpu_util(&storage, &node_id).unwrap();
        assert!((util - 0.25).abs() < 1e-9, "got {util}");
    }
}
