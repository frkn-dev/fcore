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

fn default_node_load_conn_capacity() -> u32 {
    300
}

fn default_node_load_threshold() -> f64 {
    0.85
}

fn default_pool_strict_inbounds() -> bool {
    true
}

fn default_listen_address() -> Ipv4Addr {
    "127.0.0.1".parse().unwrap()
}

fn default_log_level() -> String {
    "debug".to_string()
}

/// Directory of the public geo lists served at `/router/geo/*`. The daily
/// `frkn-geo-update` timer writes `ru.zone` (and `ru.zone.sha256`) here
/// atomically; see `docs/router-geo.md`.
fn default_geo_dir() -> String {
    "/var/lib/frkn/geo".to_string()
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

/// Peers: the client owns the key pair, the api assigns the address.
///
/// Inert until `enabled = true`: the routes are registered either way, but a
/// disabled section answers 404 so a half-configured deployment cannot hand
/// out tunnels nobody serves.
#[derive(Clone, Debug, Deserialize)]
pub struct PeersConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Default cap of live tunnels per physical device, counted by its X25519
    /// public key across every subscription and env — a phone keeps one, a
    /// router may keep three up for failover. A per-subscription override in
    /// `subscription_limits.peers_per_device` wins over this. `0` (or no key)
    /// means 3. The old name `max_per_device` is still read.
    #[serde(default = "default_peers_per_device", alias = "max_per_device")]
    pub peers_per_device: i64,
    /// Default cap of live devices (installations) per subscription.
    #[serde(default = "default_peers_max_devices_per_subscription")]
    pub max_devices_per_subscription: i64,
    /// Default cap of live tunnels per subscription (the guard that keeps one
    /// account from occupying a whole /20).
    #[serde(default = "default_peers_max_per_subscription")]
    pub max_per_subscription: i64,
    /// env → subnet blocks, seeded into `env_networks` at startup (the table
    /// stays authoritative; overlapping blocks are refused by the DB).
    #[serde(default)]
    pub subnets: HashMap<Env, Vec<String>>,
    /// Supernet the api carves shard blocks out of on first use
    /// (`10.128.0.0/9`). Needed where the number of shards is unknown — premium
    /// slots and customers' own machines: no live list of their subnets in the
    /// config, and none of them are related to each other. Empty disables it.
    #[serde(default)]
    pub auto_subnet_pool: Option<String>,
    /// Prefix of an auto-allocated shard block (default /16: one shard is one
    /// machine of one customer).
    #[serde(default = "default_auto_subnet_prefix")]
    pub auto_subnet_prefix: u8,
    /// Shards declared here are applied at startup: plan, title, kind and caps
    /// are written to `env_capacity`, so provisioning is a config edit instead
    /// of hand-written SQL. The admin endpoint writes the same row for changes
    /// that must not wait for a restart.
    #[serde(default)]
    pub shards: HashMap<Env, ShardDecl>,
    /// Revoked peers stay listed in `GET /peers` for this many days, so the
    /// app can say why a tunnel died instead of "no such peer".
    #[serde(default = "default_peers_remember_days")]
    pub remember_days: i64,
    /// A device with no live tunnel and nothing created for this many days is
    /// deleted, together with its (already revoked) peers.
    ///
    /// The device cap is per subscription and `device_uid` survives a reinstall,
    /// so without reaping the cap fills with phones that no longer exist. Zero
    /// disables the task; the first tick after startup already reaps.
    #[serde(default = "default_peers_stale_device_days")]
    pub stale_device_days: i64,
    /// Remove v1 connections whose `(env, proto)` the subscription no longer
    /// holds a grant for. Off by default: a migration must be deliberate, and
    /// until the client can build peers a pruned connection leaves the user
    /// with nothing.
    #[serde(default)]
    pub prune_legacy_conns: bool,
    /// Only log what would be pruned. On by default, so the first window after
    /// enabling pruning is read-only.
    #[serde(default = "default_true")]
    pub prune_dry_run: bool,
    /// Safety valve: more candidates than this in one tick means the grant is
    /// wrong, so nothing is deleted for that subscription and a warning is
    /// logged.
    #[serde(default = "default_peers_prune_max_per_tick")]
    pub prune_max_per_tick: usize,
}

fn default_peers_per_device() -> i64 {
    3
}

fn default_peers_max_per_subscription() -> i64 {
    // 15 devices x 3 tunnels: the peer cap must not bind before the device cap.
    45
}

fn default_peers_max_devices_per_subscription() -> i64 {
    15
}

fn default_peers_remember_days() -> i64 {
    7
}

fn default_peers_stale_device_days() -> i64 {
    90
}

fn default_auto_subnet_prefix() -> u8 {
    16
}

fn default_peers_prune_max_per_tick() -> usize {
    50
}

fn default_true() -> bool {
    true
}

impl Default for PeersConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            peers_per_device: default_peers_per_device(),
            max_per_subscription: default_peers_max_per_subscription(),
            max_devices_per_subscription: default_peers_max_devices_per_subscription(),
            subnets: HashMap::new(),
            auto_subnet_pool: None,
            auto_subnet_prefix: default_auto_subnet_prefix(),
            shards: HashMap::new(),
            remember_days: default_peers_remember_days(),
            stale_device_days: default_peers_stale_device_days(),
            prune_legacy_conns: false,
            prune_dry_run: true,
            prune_max_per_tick: default_peers_prune_max_per_tick(),
        }
    }
}

/// One shard as declared in the api config (`[service.peers.shards."0001"]`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ShardDecl {
    /// Plan whose subscribers are assigned to this shard
    /// (`standard|lite|premium|selfhosted`).
    pub plan: String,
    /// Human name shown as the group header in the app.
    #[serde(default)]
    pub title: Option<String>,
    /// `core|premium|personal`. Optional: derived from `plan` when omitted
    /// (standard/lite → core, premium → premium, selfhosted → personal), so a
    /// declaration cannot drift from the tariff it sells.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub max_subscriptions: Option<i32>,
    #[serde(default = "default_true")]
    pub enabled: bool,
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
    /// City-база: даёт город клиента («Владивосток»). Опциональна.
    #[serde(default)]
    pub geoip_city_path: Option<String>,
    /// Directory of the public geo lists served at `/router/geo/*`
    /// (`ru.zone`, `ru.zone.sha256`). Written atomically by the daily
    /// `frkn-geo-update` timer (`deploy/geo/`), see `docs/router-geo.md`.
    #[serde(default = "default_geo_dir")]
    pub geo_dir: String,
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
    /// Issue distinct AWG and AWG Mobile credentials to each installation of
    /// a self-hosted child subscription through the existing gateway.
    #[serde(default)]
    pub installation_keys_enabled: bool,
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
    /// Capacity used to turn a node's live connection count into `conn_util`
    /// for `POST /v1/nodes/load`. Nodes do not report their own limit yet, so
    /// one fleet-wide number keeps the formula honest instead of inventing
    /// per-node values.
    #[serde(default = "default_node_load_conn_capacity")]
    pub node_load_conn_capacity: u32,
    /// Above this load the endpoint reports a hard `1.0` instead of a milder
    /// number the client would happily ignore.
    #[serde(default = "default_node_load_threshold")]
    pub node_load_threshold: f64,
    /// Registration of a node whose inbounds differ from its pool peers.
    /// `true` (default) answers 409 `pool_inbounds_conflict` with the exact
    /// fields, `false` only logs a warning. Either way the node is registered
    /// and its status becomes `Degraded` — a mismatch must not cut the data
    /// plane for clients that already work.
    #[serde(default = "default_pool_strict_inbounds")]
    pub pool_strict_inbounds: bool,
    /// Peer scheme. Disabled by default: an api that has the code but no
    /// enabled section behaves exactly like before this feature.
    #[serde(default)]
    pub peers: PeersConfig,
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
    /// How often peer traffic is read from the metrics bus and persisted.
    /// Absent or `0` inherits `traffic_persist_interval_sec`, so one knob tunes
    /// both the v1 connection and the peer persister.
    #[serde(default)]
    pub peer_traffic_persist_interval_sec: Option<u64>,
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
    /// How often `[service.peers] stale_device_days` is applied. The threshold
    /// itself is counted in days, so an hourly tick is plenty.
    #[serde(default = "default_stale_device_interval_sec")]
    pub stale_device_interval_sec: u64,
}

impl TasksConfig {
    /// Cadence of the peer traffic persister: the explicit
    /// `peer_traffic_persist_interval_sec` when set, otherwise the same value
    /// the v1 connection persister uses.
    pub fn peer_traffic_interval_sec(&self) -> u64 {
        match self.peer_traffic_persist_interval_sec {
            Some(secs) if secs > 0 => secs.max(5),
            _ => self.traffic_persist_interval_sec.max(5),
        }
    }
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

/// Default cadence of traffic persistence for connections and peers alike.
/// A card's `used_bytes` may lag by one interval, so this is minutes, not
/// hours: an hourly tick left a paying user's usage ~9 KB behind after 90+ KB
/// had actually flowed.
fn default_traffic_persist_interval_sec() -> u64 {
    300
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

fn default_tombstone_republish_interval_sec() -> u64 {
    3600
}

fn default_stale_device_interval_sec() -> u64 {
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

    #[test]
    fn a_declared_shard_parses_and_keeps_its_policy() {
        let cfg: PeersConfig = toml::from_str(
            r#"
            enabled = true
            [subnets]
            "0001" = ["10.88.0.0/24"]

            [shards."0001"]
            plan = "standard"
            title = "Standard"

            [shards."1001"]
            plan = "lite"
            title = "Super Duper"
            "#,
        )
        .expect("shard declarations must parse");

        let first = cfg.shards.get(&Env::Named("0001".into())).expect("0001");
        assert_eq!(first.plan, "standard");
        // Not declared by hand: the kind follows the plan.
        assert_eq!(first.kind, None);
        assert_eq!(
            crate::peers::ShardKind::for_plan(&first.plan),
            Some(crate::peers::ShardKind::Core)
        );
        assert_eq!(
            crate::peers::ShardKind::for_plan("premium"),
            Some(crate::peers::ShardKind::Premium)
        );
        assert!(first.enabled);
        let second = cfg.shards.get(&Env::Named("1001".into())).expect("1001");
        assert_eq!(second.plan, "lite");
        assert_eq!(second.title.as_deref(), Some("Super Duper"));
    }

    fn tasks_from(toml_src: &str) -> TasksConfig {
        toml::from_str(toml_src).expect("tasks config must parse")
    }

    const TASKS_REQUIRED: &str = r#"
        db_sync_interval_sec = 1000
        subscription_restore_interval = 600
        subscription_expire_interval = 600
        connection_expire_interval = 600
        monitor_nodes_interval = 60
        heartbeat_node_offline_threshold_sec = 120
    "#;

    #[test]
    fn traffic_persist_defaults_to_300_for_connections_and_peers() {
        let cfg = tasks_from(TASKS_REQUIRED);
        assert_eq!(cfg.traffic_persist_interval_sec, 300);
        // No peer key: the peer persister follows the connection cadence.
        assert_eq!(cfg.peer_traffic_persist_interval_sec, None);
        assert_eq!(cfg.peer_traffic_interval_sec(), 300);
    }

    #[test]
    fn peer_traffic_interval_inherits_the_connection_key() {
        let cfg = tasks_from(&format!("{TASKS_REQUIRED}\ntraffic_persist_interval_sec = 600\n"));
        assert_eq!(cfg.traffic_persist_interval_sec, 600);
        assert_eq!(cfg.peer_traffic_interval_sec(), 600);
    }

    #[test]
    fn peer_traffic_interval_can_be_tuned_alone() {
        let cfg = tasks_from(&format!(
            "{TASKS_REQUIRED}\ntraffic_persist_interval_sec = 600\npeer_traffic_persist_interval_sec = 120\n"
        ));
        assert_eq!(cfg.traffic_persist_interval_sec, 600);
        assert_eq!(cfg.peer_traffic_interval_sec(), 120);

        // `0` is the documented "inherit" value, not a 0-second tick.
        let inherit = tasks_from(&format!(
            "{TASKS_REQUIRED}\ntraffic_persist_interval_sec = 600\npeer_traffic_persist_interval_sec = 0\n"
        ));
        assert_eq!(inherit.peer_traffic_interval_sec(), 600);
    }
}
