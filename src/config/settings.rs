use serde::{de::DeserializeOwned, Deserialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::net::Ipv4Addr;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::memory::{
    env::Env,
    node::{Endpoint, Type, DEFAULT_NODE_WEIGHT},
};

#[derive(Clone, Debug, Deserialize, Default)]
pub struct ApiAccessConfig {
    pub endpoint: String,
    pub token: String,
}

fn default_metrics_enabled() -> bool {
    true
}

fn default_metrics_endpoint() -> String {
    "0.0.0.0:3000".to_string()
}

#[derive(Clone, Default, Debug, Deserialize)]
pub struct MetricsTxConfig {
    pub publisher: String,
    pub interval: u64,
    #[serde(default = "default_metrics_enabled")]
    pub enabled: bool,
    #[serde(default = "default_metrics_endpoint")]
    pub endpoint: String,

    /// Токен для /healthcheck на ноде. Пусто — ручка закрыта (404).
    #[serde(default)]
    pub healthcheck_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NodeConfig {
    pub env: Env,
    pub hostname: String,
    pub default_interface: String,
    pub address: Ipv4Addr,
    pub uuid: Uuid,
    pub label: String,
    pub max_bandwidth_bps: i64,
    pub cores: usize,
    pub country: String,
    pub r#type: Type,
    pub cluster: Option<String>,
    /// Entry points of the node (`[[node.endpoints]]`), already normalised and
    /// always non-empty: a node that declares none gets one endpoint for
    /// `address`. Single source of truth for addresses, ports and weights.
    pub endpoints: Option<Vec<Endpoint>>,
    /// `true` when the config still uses the DEPRECATED `node_ips` /
    /// `node_weights` keys. Reported to the api (`/status`) so migration
    /// progress is countable before those keys are removed.
    pub endpoints_legacy: bool,
    /// DEPRECATED — use `[[node.endpoints]]`.
    /// TODO(remove-legacy-endpoints): delete once no node reports
    /// `endpoints_legacy: true` and no old api build needs the key.
    #[deprecated(
        note = "use [[node.endpoints]]; remove once no node reports endpoints_legacy"
    )]
    pub node_ips: Option<Vec<Ipv4Addr>>,
    /// Optional pool/group name (config.toml `[node] pool`). Clients group
    /// identically-named nodes and may localize the title via
    /// `pool_translation`. None = old node / no pool.
    pub pool: Option<String>,
    /// Per-language translations of `pool` (e.g. en/ru/uk).
    pub pool_translation: Option<BTreeMap<String, String>>,
    /// Country of the whole pool (`[node] pool_country_code`), not of this
    /// machine: the card shows one flag even when the pool spans countries.
    /// Normalised to 2–3 upper-case latin letters.
    pub pool_country_code: Option<String>,
    /// Human name of `pool_country_code` (`[node] pool_country_name`); absent
    /// means the client translates the code itself.
    pub pool_country_name: Option<String>,
    /// DEPRECATED — use `[[node.endpoints]]` (`weight` of each entry).
    /// TODO(remove-legacy-endpoints): delete together with `node_ips`.
    #[deprecated(
        note = "use [[node.endpoints]]; remove once no node reports endpoints_legacy"
    )]
    pub node_weights: Option<BTreeMap<String, i32>>,
    /// Base URL of the node's own speed-test endpoint (`[node] bench_url`,
    /// e.g. `https://ncr-nl-ams-bench-01.bench.beacon.frkn.org:19443`).
    /// Absent = the node serves no bench (premium slots). Registered with the
    /// api, which publishes it verbatim as `speedtest_url` in `/status`.
    pub bench_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NodeConfigRaw {
    /// The shard this node serves. New configs write `shard = "0001"`; `env`
    /// keeps working as the historical key, so existing files need no edit.
    #[serde(alias = "shard")]
    pub env: Env,
    pub hostname: Option<String>,
    pub default_interface: String,
    pub address: Ipv4Addr,
    pub uuid: Uuid,
    pub label: String,
    pub max_bandwidth_bps: i64,
    pub country: String,
    pub r#type: String,
    #[serde(default)]
    pub cluster: Option<String>,
    /// Entry points of the node (`[[node.endpoints]]`): `ip` plus optional
    /// `port`, `weight` and `label`. Present → it wins and the DEPRECATED
    /// `node_ips`/`node_weights` keys are ignored (with a warning). Absent →
    /// the legacy path runs exactly as before.
    #[serde(default)]
    pub endpoints: Option<Vec<NodeEndpointRaw>>,
    /// DEPRECATED — use `[[node.endpoints]]`.
    /// TODO(remove-legacy-endpoints): delete once no node reports
    /// `endpoints_legacy: true`. Kept parsed so old configs keep working.
    #[serde(default)]
    #[deprecated(
        note = "use [[node.endpoints]]; remove once no node reports endpoints_legacy"
    )]
    pub node_ips: Option<Vec<Ipv4Addr>>,
    /// Optional pool/group name; a blank value is treated as absent.
    #[serde(default)]
    pub pool: Option<String>,
    /// Optional map lang -> translated pool name; blank entries are dropped
    /// and `ua` is accepted as an alias of `uk`.
    #[serde(default)]
    pub pool_translation: Option<BTreeMap<String, String>>,
    /// Country of the whole pool (config.toml `[node] pool_country_code`): the
    /// flag of the card, not of this machine. Anything but 2–3 latin letters is
    /// dropped, so a typo cannot put "europe" under the flag.
    #[serde(default, alias = "pool_country")]
    pub pool_country_code: Option<String>,
    /// Human name of the pool country (`[node] pool_country_name`); optional,
    /// the client translates the code when it is absent.
    #[serde(default)]
    pub pool_country_name: Option<String>,
    /// DEPRECATED — use `[[node.endpoints]]` (`weight` of each entry).
    /// TODO(remove-legacy-endpoints): delete together with `node_ips`.
    #[serde(default)]
    #[deprecated(
        note = "use [[node.endpoints]]; remove once no node reports endpoints_legacy"
    )]
    pub node_weights: Option<BTreeMap<String, i32>>,
    /// Base URL of the node's own bench (`[node] bench_url`): where this node
    /// serves its speed-test files. Absent or blank = the node has no bench.
    #[serde(default)]
    pub bench_url: Option<String>,
}

/// One `[[node.endpoints]]` entry as written in config.toml.
///
/// `port` is `i64` on purpose: `port = 0` and `port = 70000` must produce the
/// node's own validation error instead of a serde type error that hides which
/// key is wrong.
#[derive(Clone, Debug, Deserialize)]
pub struct NodeEndpointRaw {
    pub ip: Ipv4Addr,
    #[serde(default)]
    pub port: Option<i64>,
    #[serde(default)]
    pub weight: Option<i32>,
    #[serde(default)]
    pub label: Option<String>,
}

/// Validates `[[node.endpoints]]` and returns the canonical endpoint list.
///
/// Rules (the schema is the same on every level — node config, node→api
/// announcement, api→client payload):
/// - `ip` must be a valid IPv4 and unique within the node (a duplicate means a
///   client would weigh one address twice) — a duplicate is an error;
/// - `port`, when present, must be in `1..=65535`; absent = the inbound's port;
/// - `weight <= 0` means "not set" and falls back to [`DEFAULT_NODE_WEIGHT`];
/// - `label` is trimmed, blank means absent, and is only ever used in logs;
/// - `endpoints[0].ip` must equal `address`, exactly like the legacy
///   `node_ips[0]` rule: an old client/consumer reads `address` as the primary
///   entry and would otherwise dial an address the node does not announce.
fn normalize_endpoints(
    raw: Option<Vec<NodeEndpointRaw>>,
    address: Ipv4Addr,
) -> Result<Option<Vec<Endpoint>>> {
    let Some(raw) = raw.filter(|list| !list.is_empty()) else {
        return Ok(None);
    };
    let mut seen: Vec<Ipv4Addr> = Vec::with_capacity(raw.len());
    let mut endpoints: Vec<Endpoint> = Vec::with_capacity(raw.len());
    for entry in raw {
        if seen.contains(&entry.ip) {
            return Err(Error::Custom(format!(
                "Validation error: duplicate endpoint ip {}. Every [[node.endpoints]] ip must be unique",
                entry.ip
            )));
        }
        seen.push(entry.ip);
        let port = match entry.port {
            Some(port) if (1..=65535).contains(&port) => Some(port as u16),
            Some(port) => {
                return Err(Error::Custom(format!(
                    "Validation error: endpoint {} port {port} is out of range 1..=65535",
                    entry.ip
                )));
            }
            None => None,
        };
        let weight = match entry.weight {
            Some(weight) if weight > 0 => weight,
            Some(weight) => {
                tracing::warn!(
                    ip = %entry.ip,
                    weight,
                    "config_endpoint_weight_defaulted"
                );
                DEFAULT_NODE_WEIGHT
            }
            None => DEFAULT_NODE_WEIGHT,
        };
        let label = entry
            .label
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty());
        endpoints.push(Endpoint {
            ip: entry.ip,
            port,
            weight,
            label,
        });
    }
    if endpoints[0].ip != address {
        return Err(Error::Custom(format!(
            "Validation error: endpoints[0] ({}) must equal the node address ({address})",
            endpoints[0].ip
        )));
    }
    Ok(Some(endpoints))
}

/// Validates an already-shaped endpoint list (node→api announcement, or a
/// third-party tool posting the same JSON): unique IPv4 addresses, ports in
/// `1..=65535`, positive weights, primary == `address`.
///
/// The node config goes through [`normalize_endpoints`], which applies the
/// same rules while parsing TOML; this entry point applies them to the wire
/// shape so a registration never stores a node the clients cannot dial.
pub fn validate_endpoint_list(endpoints: &[Endpoint], address: Ipv4Addr) -> Result<()> {
    if endpoints.is_empty() {
        return Ok(());
    }
    let mut seen: Vec<Ipv4Addr> = Vec::with_capacity(endpoints.len());
    for endpoint in endpoints {
        if seen.contains(&endpoint.ip) {
            return Err(Error::Custom(format!(
                "Validation error: duplicate endpoint ip {}",
                endpoint.ip
            )));
        }
        seen.push(endpoint.ip);
        if matches!(endpoint.port, Some(0)) {
            return Err(Error::Custom(format!(
                "Validation error: endpoint {} port is out of range 1..=65535",
                endpoint.ip
            )));
        }
        if endpoints[0].ip != address {
            return Err(Error::Custom(format!(
                "Validation error: endpoints[0] ({}) must equal the node address ({address})",
                endpoints[0].ip
            )));
        }
    }
    Ok(())
}

/// Keeps only usable per-IP weights: the key must parse as IPv4 (the same
/// addresses fnode lists in `node_ips`) and the weight must be positive;
/// anything else falls back to the default weight.
fn normalize_node_weights(raw: Option<BTreeMap<String, i32>>) -> Option<BTreeMap<String, i32>> {
    let map: BTreeMap<String, i32> = raw
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(ip, weight)| {
            if weight <= 0 {
                return None;
            }
            match ip.trim().parse::<Ipv4Addr>() {
                Ok(parsed) => Some((parsed.to_string(), weight)),
                Err(_) => None,
            }
        })
        .collect();
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

/// Trims the configured pool name and treats a blank value as absent.
fn normalize_pool(pool: Option<String>) -> Option<String> {
    pool.map(|p| p.trim().to_string()).filter(|p| !p.is_empty())
}

/// Pool country code (`[node] pool_country_code`): trimmed, upper-cased and
/// kept only when it is 2–3 latin letters — the flag lookup the client does
/// takes ISO codes, and a typo must not reach the card.
pub fn normalize_pool_country(code: Option<String>) -> Option<String> {
    let code = code?.trim().to_uppercase();
    let usable = (2..=3).contains(&code.chars().count())
        && code.chars().all(|c| c.is_ascii_alphabetic());
    usable.then_some(code)
}

/// Pool country name (`[node] pool_country_name`): trimmed, blank means absent.
pub fn normalize_pool_country_name(name: Option<String>) -> Option<String> {
    name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
}

/// Bench URL (`[node] bench_url`): trimmed, with any trailing slashes dropped
/// so it stays the *base* a consumer appends `/speedtest/...` to. Only
/// `http(s)://<host>[:port][/path]` survives: a typo must not publish an
/// address no client can dial. Blank = absent (the node has no bench).
pub fn normalize_bench_url(url: Option<String>) -> Option<String> {
    let trimmed = url?.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return None;
    }
    let usable = strip_scheme(&trimmed).is_some_and(|rest| {
        // Authority = everything before the path, query or fragment.
        let end = rest
            .find(|c| c == '/' || c == '?' || c == '#')
            .unwrap_or(rest.len());
        let authority = &rest[..end];
        !authority.is_empty()
            && !authority.contains('@')
            && !trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
    });
    if !usable {
        tracing::warn!(
            value = %trimmed,
            hint = "некорректный bench_url: нужен http(s)://<host>[:port][/path]",
            "config_invalid_bench_url"
        );
        return None;
    }
    Some(trimmed)
}

fn strip_scheme(url: &str) -> Option<&str> {
    url.strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
}

/// Normalizes a `pool_translation` map: trims keys/values, lowercases the
/// language code, aliases `ua` to `uk` and drops blank entries. An empty
/// result becomes `None` so a partially-filled config never wipes stored
/// translations with empty strings.
fn normalize_pool_translation(
    map: Option<BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    let cleaned: BTreeMap<String, String> = map?
        .into_iter()
        .filter_map(|(key, value)| {
            let mut key = key.trim().to_lowercase();
            if key == "ua" {
                key = "uk".to_string();
            }
            let value = value.trim().to_string();
            if key.is_empty() || value.is_empty() {
                None
            } else {
                Some((key, value))
            }
        })
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

impl NodeConfig {
    #[allow(deprecated)]
    pub fn from_raw(raw: NodeConfigRaw) -> Result<NodeConfig> {
        let num_cpus = std::thread::available_parallelism()?.get();
        let hostname = if raw.hostname.is_none() {
            match env::var("HOSTNAME") {
                Ok(hostname) => hostname,
                Err(_) => {
                    return Err(Error::Custom("Validation error: missing hostname (set $HOSTNAME env or specify in config)".into()));
                }
            }
        } else {
            raw.hostname.unwrap()
        };

        let declared = normalize_endpoints(raw.endpoints, raw.address)?;
        // A config that still carries the DEPRECATED keys keeps working
        // unchanged; it is only counted (`endpoints_legacy`) and warned about,
        // so `journalctl` shows which machine is not migrated yet.
        let legacy_used = raw.node_ips.is_some() || raw.node_weights.is_some();
        let (node_ips, node_weights, endpoints, endpoints_legacy) = match declared {
            Some(endpoints) => {
                if legacy_used {
                    tracing::warn!(
                        address = %raw.address,
                        "config_legacy_endpoints_ignored: [[node.endpoints]] is present, node_ips/node_weights are ignored"
                    );
                }
                // Legacy mirror, derived from the new section so every old
                // consumer (old api build, old client) still sees the fields
                // it has always read.
                let ips: Vec<Ipv4Addr> = endpoints.iter().map(|endpoint| endpoint.ip).collect();
                let node_ips = if ips.len() > 1 { Some(ips) } else { None };
                let weights: BTreeMap<String, i32> = endpoints
                    .iter()
                    .filter(|endpoint| endpoint.weight != DEFAULT_NODE_WEIGHT)
                    .map(|endpoint| (endpoint.ip.to_string(), endpoint.weight))
                    .collect();
                let node_weights = if weights.is_empty() {
                    None
                } else {
                    Some(weights)
                };
                (node_ips, node_weights, Some(endpoints), false)
            }
            None => {
                if legacy_used {
                    tracing::warn!(
                        address = %raw.address,
                        "deprecated legacy endpoints: node_ips/node_weights — migrate to [[node.endpoints]] (env/dev/scripts/migrate-node-endpoints.py)"
                    );
                }
                let node_ips = raw.node_ips.clone();
                let node_weights = normalize_node_weights(raw.node_weights.clone());
                // Same shape as an explicit `[[node.endpoints]]` section: the
                // rest of the node only ever reads `endpoints`.
                let ips: Vec<Ipv4Addr> = node_ips
                    .clone()
                    .filter(|ips| !ips.is_empty())
                    .unwrap_or_else(|| vec![raw.address]);
                let endpoints: Vec<Endpoint> = ips
                    .iter()
                    .map(|ip| Endpoint {
                        ip: *ip,
                        port: None,
                        weight: node_weights
                            .as_ref()
                            .and_then(|weights| weights.get(&ip.to_string()).copied())
                            .unwrap_or(DEFAULT_NODE_WEIGHT),
                        label: None,
                    })
                    .collect();
                (node_ips, node_weights, Some(endpoints), legacy_used)
            }
        };

        Ok(NodeConfig {
            env: raw.env,
            hostname,
            default_interface: raw.default_interface,
            address: raw.address,
            uuid: raw.uuid,
            label: raw.label,
            max_bandwidth_bps: raw.max_bandwidth_bps,
            cores: num_cpus,
            country: raw.country,
            r#type: raw.r#type.parse().unwrap_or(Type::Node),
            cluster: raw.cluster,
            endpoints,
            endpoints_legacy,
            node_ips,
            pool: normalize_pool(raw.pool),
            pool_translation: normalize_pool_translation(raw.pool_translation),
            pool_country_code: normalize_pool_country(raw.pool_country_code),
            pool_country_name: normalize_pool_country_name(raw.pool_country_name),
            node_weights,
            bench_url: normalize_bench_url(raw.bench_url),
        })
    }
}

/// Предупреждает о ключах, которые похожи на настройку логирования, но ею не
/// являются: `log_velel`, `log-level`, `loglevel`. Опечатка в таком ключе
/// молча оставляет уровень по умолчанию — на self-hosted ноде это привело к
/// debug и печати приватных ключей в journalctl.
/// Возвращает ключи, похожие на настройку уровня логирования, но ею не
/// являющиеся. Вынесено отдельно, чтобы это можно было покрыть тестом.
fn suspicious_level_keys(config_str: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in config_str.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.starts_with('[') {
            continue;
        }
        let Some((key, _value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let lowered = key.to_ascii_lowercase();
        // Именно starts_with("log"): опечатка `log_velel` слова «level» не
        // содержит, а `contains("level")` её пропускал.
        if lowered.starts_with("log") && key != "log_level" {
            out.push(key.to_string());
        }
    }
    out
}

#[cfg(test)]
mod log_key_tests {
    use super::suspicious_level_keys;

    #[test]
    fn typo_in_log_level_is_detected() {
        let cfg = "[service]\nlog_velel = \"info\"\n";
        assert_eq!(suspicious_level_keys(cfg), vec!["log_velel".to_string()]);
    }

    #[test]
    fn correct_key_and_comments_are_ignored() {
        let cfg = "# log_velel = \"debug\"\n[service]\nlog_level = \"info\"\n";
        assert!(suspicious_level_keys(cfg).is_empty());
    }
}

fn warn_suspicious_level_keys(config_str: &str) {
    for (line_no, line) in config_str.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.starts_with('[') {
            continue;
        }
        let Some((key, _value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let lowered = key.to_ascii_lowercase();
        if lowered.contains("level") && key != "log_level" {
            tracing::warn!(
                key = %key,
                line = line_no + 1,
                hint = "неизвестный ключ уровня логирования: опечатка? рабочий ключ — log_level",
                "config_unknown_key"
            );
        }
    }
}

pub trait Settings: Sized {
    fn read_config<T: DeserializeOwned>(config_file: &str) -> Result<T> {
        let config_str = fs::read_to_string(config_file)?;
        let settings: T = toml::from_str(&config_str)?;
        warn_suspicious_level_keys(&config_str);
        Ok(settings)
    }

    fn from_file(config_file: &str) -> Self
    where
        for<'de> Self: Deserialize<'de>,
    {
        match Self::read_config(config_file) {
            Ok(settings) => settings,
            Err(e) => panic!("Failed to load settings: {}", e),
        }
    }

    fn validate(&self) -> Result<()>;
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    /// Parses the same layout the fleet uses: the node settings live in the
    /// `[node]` table of config.toml, so `[[node.endpoints]]` is nested in it.
    fn parse_node(text: &str) -> Result<NodeConfig> {
        #[derive(Deserialize)]
        struct Doc {
            node: NodeConfigRaw,
        }
        let doc: Doc = toml::from_str(text).expect("test toml parses");
        NodeConfig::from_raw(doc.node)
    }

    #[test]
    fn pool_translation_normalizes_ua_and_blanks() {
        let mut raw = BTreeMap::new();
        raw.insert(" UA ".to_string(), " Фінляндія ".to_string());
        raw.insert("ru".to_string(), "   ".to_string());
        raw.insert("en".to_string(), "Finland".to_string());

        let normalized = normalize_pool_translation(Some(raw)).expect("non-empty map");
        assert_eq!(normalized.get("uk").map(String::as_str), Some("Фінляндія"));
        assert_eq!(normalized.get("en").map(String::as_str), Some("Finland"));
        assert!(!normalized.contains_key("ru"));
        assert!(!normalized.contains_key("ua"));
    }

    #[test]
    fn pool_translation_empty_or_blank_is_none() {
        assert!(normalize_pool_translation(None).is_none());
        assert!(normalize_pool_translation(Some(BTreeMap::new())).is_none());
    }

    #[test]
    fn pool_blank_becomes_none() {
        assert!(normalize_pool(Some("   ".to_string())).is_none());
        assert_eq!(
            normalize_pool(Some(" Finland ".to_string())),
            Some("Finland".to_string())
        );
    }

    /// The card's flag is looked up by ISO code: anything that is not 2-3 latin
    /// letters must be dropped rather than shown as a broken flag.
    #[test]
    fn pool_country_code_takes_only_iso_codes() {
        assert_eq!(
            normalize_pool_country(Some(" nl ".to_string())).as_deref(),
            Some("NL")
        );
        assert_eq!(
            normalize_pool_country(Some("usa".to_string())).as_deref(),
            Some("USA")
        );
        assert!(normalize_pool_country(Some("europe".to_string())).is_none());
        assert!(normalize_pool_country(Some("N1".to_string())).is_none());
        assert!(normalize_pool_country(Some("   ".to_string())).is_none());
        assert!(normalize_pool_country(None).is_none());
    }

    #[test]
    fn pool_country_name_is_trimmed_or_absent() {
        assert_eq!(
            normalize_pool_country_name(Some(" Netherlands ".to_string())).as_deref(),
            Some("Netherlands")
        );
        assert!(normalize_pool_country_name(Some("  ".to_string())).is_none());
    }

    /// The bench URL is published to clients verbatim, who append
    /// `/speedtest/...`: a trailing slash would double it, and a value without
    /// http(s) and a host is not an address anyone can dial.
    #[test]
    fn bench_url_keeps_only_a_dialable_base() {
        assert_eq!(
            normalize_bench_url(Some(
                " https://ncr-nl-ams-bench-01.bench.beacon.frkn.org:19443/ ".to_string()
            ))
            .as_deref(),
            Some("https://ncr-nl-ams-bench-01.bench.beacon.frkn.org:19443")
        );
        assert_eq!(
            normalize_bench_url(Some("http://10.0.0.5:8080".to_string())).as_deref(),
            Some("http://10.0.0.5:8080")
        );
        // Absent / blank = "no bench", not an empty string on the wire.
        assert!(normalize_bench_url(None).is_none());
        assert!(normalize_bench_url(Some("   ".to_string())).is_none());
        assert!(normalize_bench_url(Some("///".to_string())).is_none());
        // No scheme, no host, a path-only value or a stray space: dropped.
        assert!(normalize_bench_url(Some("ncr-nl-ams-bench-01:19443".to_string())).is_none());
        assert!(normalize_bench_url(Some("https://".to_string())).is_none());
        assert!(normalize_bench_url(Some("https:///speedtest".to_string())).is_none());
        assert!(normalize_bench_url(Some("https://host with space".to_string())).is_none());
        assert!(normalize_bench_url(Some("ftp://host".to_string())).is_none());
        assert!(normalize_bench_url(Some("https://user@host".to_string())).is_none());
    }

    // ---------------------------------------------------------- endpoints

    /// Real EU node config (env/dev/servers/fr.frkn.org/config.toml) with the
    /// legacy multi-IP list: (a) the parsed result must be exactly what the
    /// pre-endpoints code produced — same `node_ips`, same weights, no ports.
    #[test]
    fn legacy_multi_ip_config_parses_as_before() {
        let config = parse_node(
            r#"
[node]
env = "dev"
hostname = "fr.frkn.org"
default_interface = "net0"
address = "77.221.153.189"
uuid = "85a5b650-4a3c-4ec1-94bf-21a894760aaf"
label = "France AWG3.1"
max_bandwidth_bps = 200000000
country = "FR"
type = "Common"
node_ips = [
"77.221.153.189",
"62.60.248.235",
"62.60.246.138",
"109.120.178.116",
]
"#,
        )
        .expect("real EU node config must parse");

        assert_eq!(
            config.node_ips,
            Some(vec![
                "77.221.153.189".parse().unwrap(),
                "62.60.248.235".parse().unwrap(),
                "62.60.246.138".parse().unwrap(),
                "109.120.178.116".parse().unwrap(),
            ]),
            "the legacy IP list must be preserved verbatim"
        );
        assert!(
            config.node_weights.is_none(),
            "no node_weights in the file -> None, as before"
        );
        assert!(
            config.endpoints_legacy,
            "a config on the legacy keys must be reported as legacy"
        );
        // The single shape the rest of the node reads is derived from the
        // legacy keys: same addresses, default weight, no port override.
        let endpoints = config.endpoints.expect("endpoints are always populated");
        assert_eq!(endpoints.len(), 4);
        assert_eq!(endpoints[1].ip, "62.60.248.235".parse::<Ipv4Addr>().unwrap());
        assert_eq!(endpoints[1].port, None);
        assert_eq!(endpoints[1].weight, DEFAULT_NODE_WEIGHT);
    }

    /// Real RU config with the legacy weights map.
    #[test]
    fn legacy_weights_reach_the_endpoints_mirror() {
        let config = parse_node(
            r#"
[node]
env = "ru"
hostname = "msk-tw.frkn.org"
default_interface = "eth0"
address = "217.149.29.118"
uuid = "447b2ee2-0351-4af6-b3eb-70341194bc81"
label = "RU Рилзопровод"
max_bandwidth_bps = 1000000000
country = "RU"
type = "Common"
node_ips = ["217.149.29.118", "95.163.1.10"]
node_weights = { "217.149.29.118" = 75, "95.163.1.10" = 25 }
"#,
        )
        .expect("RU config parses");

        let weights = config.node_weights.clone().expect("weights kept");
        assert_eq!(weights.get("217.149.29.118"), Some(&75));
        assert_eq!(weights.get("95.163.1.10"), Some(&25));
        assert!(config.endpoints_legacy);
        let endpoints = config.endpoints.expect("endpoints populated");
        assert_eq!(endpoints[0].weight, 75);
        assert_eq!(endpoints[1].weight, 25);
    }

    /// New section present -> it wins; the deprecated keys are ignored (the
    /// warning is logged, the values are not merged).
    #[test]
    fn endpoints_section_wins_over_legacy_keys() {
        let config = parse_node(
            r#"
[node]
env = "nl"
hostname = "nl-01.frkn.org"
default_interface = "eth0"
address = "82.148.231.161"
uuid = "11111111-2222-3333-4444-555555555555"
label = "NL"
max_bandwidth_bps = 1000000000
country = "NL"
type = "Common"
node_ips = ["82.148.231.161", "82.148.231.162"]
node_weights = { "82.148.231.161" = 75 }

[[node.endpoints]]
ip = "82.148.231.161"
port = 8443
weight = 200
label = "nl-01"

[[node.endpoints]]
ip = "82.148.231.162"
"#,
        )
        .expect("config with both shapes parses");

        let endpoints = config.endpoints.expect("endpoints from the new section");
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0].port, Some(8443));
        assert_eq!(endpoints[0].weight, 200);
        assert_eq!(endpoints[0].label.as_deref(), Some("nl-01"));
        assert_eq!(endpoints[1].weight, DEFAULT_NODE_WEIGHT);
        // Mirror derived from the WINNING source, not from the stale keys.
        assert_eq!(config.node_weights.as_ref().and_then(|w| w.get("82.148.231.161")), Some(&200));
        assert!(!config.endpoints_legacy, "a node on [[node.endpoints]] is migrated");
    }

    #[test]
    fn duplicate_endpoint_ip_is_an_error() {
        let config = parse_node(
            r#"
[node]
env = "dev"
hostname = "node.local"
default_interface = "eth0"
address = "10.0.0.1"
uuid = "11111111-2222-3333-4444-555555555555"
label = "x"
max_bandwidth_bps = 1
country = "NL"
type = "Common"

[[node.endpoints]]
ip = "10.0.0.1"

[[node.endpoints]]
ip = "10.0.0.1"
"#,
        );
        let err = config.expect_err("duplicate ip must be rejected");
        assert!(
            format!("{err}").contains("duplicate endpoint ip"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn out_of_range_endpoint_port_is_an_error() {
        for port in ["0", "70000"] {
            let config = parse_node(&format!(
                r#"
[node]
env = "dev"
hostname = "node.local"
default_interface = "eth0"
address = "10.0.0.1"
uuid = "11111111-2222-3333-4444-555555555555"
label = "x"
max_bandwidth_bps = 1
country = "NL"
type = "Common"

[[node.endpoints]]
ip = "10.0.0.1"
port = {port}
"#
            ));
            let err = config.expect_err("port out of range must be rejected");
            assert!(format!("{err}").contains("out of range"), "{err}");
        }
    }

    /// `weight <= 0` is "not set" (the same rule the legacy map uses), and the
    /// primary endpoint must be the node address.
    #[test]
    fn endpoint_weight_defaults_and_primary_must_match_address() {
        let config = parse_node(
            r#"
[node]
env = "dev"
hostname = "node.local"
default_interface = "eth0"
address = "10.0.0.1"
uuid = "11111111-2222-3333-4444-555555555555"
label = "x"
max_bandwidth_bps = 1
country = "NL"
type = "Common"

[[node.endpoints]]
ip = "10.0.0.1"
weight = 0
"#,
        );
        assert_eq!(config.expect("valid config").endpoints.unwrap()[0].weight, DEFAULT_NODE_WEIGHT);

        let config = parse_node(
            r#"
[node]
env = "dev"
hostname = "node.local"
default_interface = "eth0"
address = "10.0.0.1"
uuid = "11111111-2222-3333-4444-555555555555"
label = "x"
max_bandwidth_bps = 1
country = "NL"
type = "Common"

[[node.endpoints]]
ip = "10.0.0.2"
"#,
        );
        let err = config.expect_err("wrong primary must be rejected");
        assert!(format!("{err}").contains("must equal the node address"), "{err}");
    }

    /// A single-address node with neither shape still gets one endpoint and is
    /// NOT counted as legacy: there is nothing to migrate.
    #[test]
    fn single_address_node_is_not_legacy() {
        let config = parse_node(
            r#"
[node]
env = "dev"
hostname = "node.local"
default_interface = "eth0"
address = "10.0.0.1"
uuid = "11111111-2222-3333-4444-555555555555"
label = "x"
max_bandwidth_bps = 1
country = "NL"
type = "Common"
"#,
        );
        let config = config.expect("valid config");
        assert!(!config.endpoints_legacy);
        assert!(config.node_ips.is_none());
        let endpoints = config.endpoints.unwrap();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].ip, "10.0.0.1".parse::<Ipv4Addr>().unwrap());
    }
}
