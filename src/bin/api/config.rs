use serde::Deserialize;
use std::collections::HashMap;
use std::net::Ipv4Addr;

use fcore::{Env, IpAddrMask, Result, Settings, Tag};

#[derive(Clone, Debug, Deserialize)]
pub struct ServiceSettings {
    pub service: ServiceConfig,
    pub pg: PostgresConfig,
    pub metrics: MetricsRxConfig,
    pub tasks: TasksConfig,
    pub subscription_audit: SubscriptionAuditConfig,
    #[serde(default)]
    pub score: fcore::score::ScoreConfig,
    /// Правила оповещений (нода офлайн / вернулась).
    #[serde(default)]
    pub alerts: crate::alerts::AlertsConfig,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MrktingConfig {
    pub endpoint: String,
    pub token: String,
}

fn default_apple_allowed_products() -> Vec<String> {
    vec![
        "frkn_premium_1_month".to_string(),
        "frkn_premium_3_month".to_string(),
        "frkn_premium_6_month".to_string(),
        "frkn_premium_12_month".to_string(),
    ]
}

/// App Store IAP (App Store Server API) configuration.
/// Without this section the service starts normally, but `POST /v1/subscriptions` answers 503.
#[derive(Clone, Debug, Deserialize)]
pub struct AppleConfig {
    pub key_id: String,
    pub issuer_id: String,
    /// Path to the App Store Connect .p8 private key (PKCS#8 PEM).
    pub private_key_path: String,
    pub bundle_id: String,
    /// "Sandbox" or "Production".
    pub environment: String,
    /// Required when environment = "Production".
    pub app_apple_id: Option<i64>,
    /// Path to the Apple Root CA certificate (DER), e.g. AppleRootCA-G3.cer.
    pub root_ca_path: String,
    /// Product IDs allowed for binding. An empty list disables the restriction.
    #[serde(default = "default_apple_allowed_products")]
    pub allowed_products: Vec<String>,
}

impl Settings for ServiceSettings {
    fn validate(&self) -> Result<()> {
        let key_path = self
            .service
            .agw_private_key_path
            .as_ref()
            .map(|p| p.trim())
            .unwrap_or_default();
        if key_path.is_empty() {
            return Err(fcore::Error::Custom(
                "service.agw_private_key_path is required".to_string(),
            ));
        }

        // Tariff entitlements: refuse to start on an inconsistent table —
        // otherwise the mistake only shows up as a failed activation.
        validate_entitlements(
            self.service.enabled_conns.as_ref(),
            &self.service.env_proto_templates.clone().unwrap_or_default(),
        )?;

        Ok(())
    }
}

/// Checks the tariff entitlement tables:
///
/// * the classic map (`enabled_conns`) must not contain the product-only envs
///   — every `standard` subscription would silently receive them;
/// * a listed protocol template must not be empty (typo protection).
///
/// A *missing* template for an extra env is allowed here on purpose: it only
/// breaks the products that need that env, and it must not keep the whole api
/// from starting (e.g. a deployment that does not sell `gaming` yet). Such an
/// activation fails loudly at request time (500, the key is not consumed).
fn validate_entitlements(
    enabled_conns: Option<&HashMap<Env, Vec<Tag>>>,
    templates: &HashMap<Env, Vec<Tag>>,
) -> Result<()> {
    for env in [Env::Gaming, Env::BsPrem] {
        if enabled_conns.is_some_and(|enabled| enabled.contains_key(&env)) {
            return Err(fcore::Error::Custom(format!(
                "service.enabled_conns must not contain env '{}': every standard subscription \
                 would receive it",
                env
            )));
        }
    }

    for (env, tags) in templates {
        if tags.is_empty() {
            return Err(fcore::Error::Custom(format!(
                "service.env_proto_templates['{}'] is empty: name the protocols of env '{}'",
                env, env
            )));
        }
    }

    Ok(())
}

fn default_cors_origins() -> Vec<String> {
    vec!["http://localhost:8080".to_string()]
}

fn default_wg_network() -> IpAddrMask {
    "10.0.0.0/8".parse().unwrap()
}

fn default_listen_address() -> Ipv4Addr {
    "127.0.0.1".parse().unwrap()
}

fn default_log_level() -> String {
    "debug".to_string()
}

/// Protocols with per-connection traffic accounting; the only ones allowed
/// while a subscription runs on its traffic balance (traffic mode).
fn default_metered_conns() -> Vec<String> {
    [
        "Wireguard",
        "AmneziaWg",
        "AmneziaWgMobile",
        "VlessTcpReality",
        "VlessGrpcReality",
        "VlessXhttpCdn",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[derive(Clone, Debug, Deserialize)]
pub struct ServiceConfig {
    #[serde(default = "default_listen_address")]
    pub listen: Ipv4Addr,
    pub port: u16,
    pub token: String,
    pub key_sign_token: Vec<u8>,
    #[serde(default = "default_cors_origins")]
    pub cors_origins: Vec<String>,
    #[serde(default = "default_wg_network")]
    pub wireguard_network: IpAddrMask,
    #[serde(default = "default_wg_network")]
    pub amnezia_wireguard_network: IpAddrMask,
    /// Dedicated pool for AmneziaWgMobile (e.g. 10.77.0.0/16) so mobile
    /// clients avoid the CGNAT range. Without it AmneziaWgMobile
    /// connections cannot be created.
    #[serde(default)]
    pub amnezia_wireguard_mobile_network: Option<IpAddrMask>,
    /// Оффлайн-базы MaxMind для телеметрии блокировок: ASN (обязательна) и
    /// страна (опциональна). Пустое значение — обогащение выключено.
    #[serde(default)]
    pub geoip_asn_path: Option<String>,
    #[serde(default)]
    pub geoip_country_path: Option<String>,
    #[serde(default)]
    pub enabled_conns: Option<HashMap<Env, Vec<Tag>>>,
    /// Connections created for lite (traffic-only) key activations, same
    /// env → tags shape as enabled_conns. Falls back to enabled_conns when
    /// unset, so lite subs get the full default set until dedicated lite
    /// servers exist.
    #[serde(default)]
    pub lite_enabled_conns: Option<HashMap<Env, Vec<Tag>>>,
    /// Protocols of the envs added by the tariff products (`gaming`,
    /// `bs-prem`): `env → tags`. The product → env contract itself lives in
    /// [`fcore::Product`] (see `includes_standard` / `extra_envs`); this table
    /// only says how those envs are served. An env with an empty list is a
    /// configuration error; a missing one makes activation of that product
    /// fail loudly (500, the key is not consumed) instead of handing out a
    /// partial subscription.
    #[serde(default)]
    pub env_proto_templates: Option<HashMap<Env, Vec<Tag>>>,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    pub updates_endpoint_zmq: String,
    pub subscription_title: String,
    pub support_contact: String,
    pub base_url: String,
    /// Public base URL of this api (feed links like /sub/<token>). Falls
    /// back to base_url when unset.
    #[serde(default)]
    pub api_url: Option<String>,
    #[serde(default)]
    pub admin_enabled: bool,
    pub admin_token: Option<String>,
    #[serde(default)]
    pub agw_private_key_path: Option<String>,
    #[serde(default)]
    pub mrkting: Option<MrktingConfig>,
    #[serde(default)]
    pub apple: Option<AppleConfig>,
    /// Labels shown on service cards in the client (`/v1/services`).
    #[serde(default)]
    pub gateway_price_label: Option<String>,
    #[serde(default)]
    pub gateway_speed_label: Option<String>,
    /// Master switch for the traffic-balance model (beta): a subscription
    /// with a traffic limit stays active while it has bytes left, even after
    /// its paid time has expired. Default off = legacy behavior.
    #[serde(default)]
    pub traffic_mode_enabled: bool,
    /// Protocol tags allowed while a subscription is in traffic mode.
    #[serde(default = "default_metered_conns")]
    pub metered_conns: Vec<String>,
    /// Path of the mesh registry rkyv snapshot (UIN → device pubkeys). The
    /// mesh inbox itself is memory-only by design.
    #[serde(default = "default_mesh_snapshot_path")]
    pub mesh_snapshot_path: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct PostgresConfig {
    pub host: String,
    pub port: u16,
    pub db: String,
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct TasksConfig {
    pub db_sync_interval_sec: u64,
    pub subscription_restore_interval: u64,
    pub subscription_expire_interval: u64,
    pub connection_expire_interval: u64,
    pub monitor_nodes_interval: u64,
    pub heartbeat_node_offline_threshold_sec: u64,
    #[serde(default = "default_traffic_persist_interval_sec")]
    pub traffic_persist_interval_sec: u64,
    /// How often lite subscriptions are checked against their traffic limit.
    #[serde(default = "default_traffic_limit_interval_sec")]
    pub traffic_limit_interval_sec: u64,
    /// Reissue connections of lite subscriptions that stay above the
    /// per-connection device limit for too many consecutive ticks.
    #[serde(default)]
    pub device_limit_enabled: bool,
    #[serde(default = "default_device_limit_interval_sec")]
    pub device_limit_interval_sec: u64,
    #[serde(default = "default_device_limit_max_ticks")]
    pub device_limit_max_ticks: u32,
    /// How often subscriptions are reconciled against enabled_conns /
    /// lite_enabled_conns: any missing (env, proto) connection is created.
    #[serde(default = "default_conns_reconcile_interval_sec")]
    pub conns_reconcile_interval_sec: u64,
    /// How often ZMQ delete messages are re-published for recently deleted
    /// connections (heals nodes that missed the one-shot delete).
    #[serde(default = "default_tombstone_republish_interval_sec")]
    pub tombstone_republish_interval_sec: u64,
}

impl ServiceConfig {
    /// Everything needed to decide which shared connections a subscription
    /// gets: legacy/traffic maps plus the product entitlements.
    pub fn conn_resolver(&self) -> crate::entitlements::ConnResolver {
        crate::entitlements::ConnResolver {
            enabled_conns: self.enabled_conns.clone(),
            lite_enabled_conns: self.lite_enabled_conns.clone(),
            env_proto_templates: self.env_proto_templates.clone().unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MetricsLogConfig {
    pub enabled: bool,
    pub directory: String,
    pub file: String,
    pub rotation: String,
    pub level: String,
}

impl Default for MetricsLogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            directory: "logs".to_string(),
            file: "metrics.log".to_string(),
            rotation: "daily".to_string(),
            level: "debug".to_string(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct SubscriptionAuditConfig {
    pub enabled: bool,
    pub directory: String,
    pub file: String,
    pub rotation: String,
    pub level: String,
}

impl Default for SubscriptionAuditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            directory: "logs".to_string(),
            file: "subscription_audit.log".to_string(),
            rotation: "daily".to_string(),
            level: "info".to_string(),
        }
    }
}

#[derive(Clone, Default, Debug, Deserialize)]
pub struct MetricsRxConfig {
    pub reciever: String,
    pub max_points: usize,
    pub retention_seconds: i64,
    #[serde(default)]
    pub log: MetricsLogConfig,
    pub snapshot_path: String,
}

fn default_traffic_persist_interval_sec() -> u64 {
    3600
}

fn default_traffic_limit_interval_sec() -> u64 {
    300
}

fn default_device_limit_interval_sec() -> u64 {
    60
}

fn default_device_limit_max_ticks() -> u32 {
    3
}

fn default_conns_reconcile_interval_sec() -> u64 {
    3600
}

fn default_mesh_snapshot_path() -> String {
    "/var/lib/fcore/api/mesh-registry.bin".to_string()
}

fn default_tombstone_republish_interval_sec() -> u64 {
    3600
}

#[cfg(test)]
mod tests {
    use super::*;

    fn templates() -> HashMap<Env, Vec<Tag>> {
        let mut map = HashMap::new();
        map.insert(Env::Gaming, vec![Tag::AmneziaWg]);
        map.insert(Env::BsPrem, vec![Tag::AmneziaWg]);
        map
    }

    #[test]
    fn accepts_a_consistent_table() {
        assert!(validate_entitlements(None, &templates()).is_ok());
        // A deployment that does not sell the extra envs yet may omit them.
        assert!(validate_entitlements(None, &HashMap::new()).is_ok());
    }

    #[test]
    fn rejects_empty_template() {
        let mut empty = templates();
        empty.insert(Env::Gaming, vec![]);
        assert!(validate_entitlements(None, &empty).is_err());
    }

    #[test]
    fn rejects_gaming_or_bs_prem_in_classic_map() {
        let mut enabled = HashMap::new();
        enabled.insert(Env::Gaming, vec![Tag::AmneziaWg]);
        assert!(validate_entitlements(Some(&enabled), &templates()).is_err());

        // A plain classic map (dev/ru/wl only) is fine.
        let mut ok = HashMap::new();
        ok.insert(Env::Dev, vec![Tag::Hysteria2]);
        ok.insert(Env::Wl, vec![Tag::VlessXhttpCdn]);
        assert!(validate_entitlements(Some(&ok), &templates()).is_ok());
    }
}
