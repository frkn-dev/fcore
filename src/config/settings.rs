use serde::{de::DeserializeOwned, Deserialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::net::Ipv4Addr;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::memory::{env::Env, node::Type};

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
    /// Extra entry IPs of the node (node_ips[0] == `address`); None =
    /// single-address node.
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
    /// Per-entry weights (config.toml `[node] node_weights`), keyed by an
    /// address from `node_ips`; an unlisted address weighs 100.
    pub node_weights: Option<BTreeMap<String, i32>>,
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
    /// Extra entry IPs: the first must equal `address` (primary first);
    /// all IPs share the same ports/keys/inbounds. Absent = single address.
    #[serde(default)]
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
    /// Per-entry weights (config.toml `[node] node_weights`), keyed by address.
    /// An empty map means every address uses the default (100). An unparsable
    /// key or a non-positive weight is dropped.
    #[serde(default)]
    pub node_weights: Option<BTreeMap<String, i32>>,
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
            node_ips: raw.node_ips,
            pool: normalize_pool(raw.pool),
            pool_translation: normalize_pool_translation(raw.pool_translation),
            pool_country_code: normalize_pool_country(raw.pool_country_code),
            pool_country_name: normalize_pool_country_name(raw.pool_country_name),
            node_weights: normalize_node_weights(raw.node_weights),
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
mod tests {
    use super::*;

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
}
