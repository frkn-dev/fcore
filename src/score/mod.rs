use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::memory::node::Node;
use crate::metrics::{MetricEnvelope, MetricPoint};
use crate::MetricStorage;

pub const SERIES_TOTAL: &str = "node.score.total";
pub const SERIES_EWMA_1H: &str = "node.score.ewma_1h";
pub const SERIES_EWMA_24H: &str = "node.score.ewma_24h";
pub const SERIES_LOAD: &str = "node.score.load";
pub const SERIES_USERS: &str = "node.score.users";
pub const SERIES_BW_CAPACITY: &str = "node.score.bw_capacity";
pub const SERIES_BW_PER_USER: &str = "node.score.bw_per_user";
pub const SERIES_RELIABILITY: &str = "node.score.reliability";
pub const SERIES_UPTIME: &str = "node.score.uptime";

const TAU_5M_SECS: f64 = 5.0 * 60.0;
const TAU_1H_SECS: f64 = 60.0 * 60.0;
const TAU_24H_SECS: f64 = 24.0 * 60.0 * 60.0;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn default_w_load() -> f64 {
    0.25
}
fn default_w_users() -> f64 {
    0.20
}
fn default_w_bw_capacity() -> f64 {
    0.15
}
fn default_w_bw_per_user() -> f64 {
    0.15
}
fn default_w_reliability() -> f64 {
    0.15
}
fn default_w_uptime() -> f64 {
    0.10
}
fn default_soft_cap() -> f64 {
    100.0
}
fn default_link_capacity() -> f64 {
    125_000_000.0
}
fn default_target_bps() -> f64 {
    625_000.0
}
fn default_stale_after_secs() -> u64 {
    600
}
fn default_interval_secs() -> u64 {
    60
}
fn default_heartbeat_every_secs() -> u64 {
    60
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ScoreConfig {
    #[serde(default = "default_w_load")]
    pub w_load: f64,
    #[serde(default = "default_w_users")]
    pub w_users: f64,
    #[serde(default = "default_w_bw_capacity")]
    pub w_bw_capacity: f64,
    #[serde(default = "default_w_bw_per_user")]
    pub w_bw_per_user: f64,
    #[serde(default = "default_w_reliability")]
    pub w_reliability: f64,
    #[serde(default = "default_w_uptime")]
    pub w_uptime: f64,
    #[serde(default = "default_soft_cap")]
    pub soft_cap: f64,
    #[serde(default = "default_link_capacity")]
    pub link_capacity: f64,
    #[serde(default = "default_target_bps")]
    pub target_bps: f64,
    #[serde(default = "default_stale_after_secs")]
    pub stale_after_secs: u64,
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    #[serde(default = "default_heartbeat_every_secs")]
    pub heartbeat_every_secs: u64,
}

impl Default for ScoreConfig {
    fn default() -> Self {
        Self {
            w_load: default_w_load(),
            w_users: default_w_users(),
            w_bw_capacity: default_w_bw_capacity(),
            w_bw_per_user: default_w_bw_per_user(),
            w_reliability: default_w_reliability(),
            w_uptime: default_w_uptime(),
            soft_cap: default_soft_cap(),
            link_capacity: default_link_capacity(),
            target_bps: default_target_bps(),
            stale_after_secs: default_stale_after_secs(),
            interval_secs: default_interval_secs(),
            heartbeat_every_secs: default_heartbeat_every_secs(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NodeView {
    pub id: Uuid,
    pub hostname: String,
    pub label: String,
    pub cores: usize,
    pub interface: String,
    pub max_bandwidth_bps: i64,
    pub created_at_ms: i64,
}

impl From<&Node> for NodeView {
    fn from(node: &Node) -> Self {
        Self {
            id: node.uuid,
            hostname: node.hostname.clone(),
            label: node.label.clone(),
            cores: node.cores,
            interface: node.interface.clone(),
            max_bandwidth_bps: node.max_bandwidth_bps,
            created_at_ms: node.created_at.timestamp_millis(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Inputs {
    pub load1: f64,
    pub load15: f64,
    pub cores: usize,
    pub users: f64,
    pub tx_bps: f64,
    pub link_capacity: f64,
    pub heartbeat_points_24h: u64,
    pub uptime_days: f64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Components {
    pub load: f64,
    pub users: f64,
    pub bw_capacity: f64,
    pub bw_per_user: f64,
    pub reliability: f64,
    pub uptime: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct NodeScore {
    pub node_id: Uuid,
    pub hostname: String,
    pub label: String,
    pub score: f64,
    pub stale: bool,
    pub tx_bps: f64,
    pub rx_bps: f64,
    pub users: f64,
    pub per_user_bps: f64,
    pub load1: f64,
    pub components: Components,
    #[serde(skip)]
    pub raw: f64,
}

#[derive(Clone, Debug)]
pub struct Ewma {
    pub m5: f64,
    pub h1: f64,
    pub h24: f64,
    pub at_ms: i64,
}

pub fn clamp01(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    value.clamp(0.0, 1.0)
}

pub fn ewma_step(prev: f64, sample: f64, dt_secs: f64, tau_secs: f64) -> f64 {
    if tau_secs <= 0.0 || dt_secs <= 0.0 {
        return sample;
    }
    let alpha = 1.0 - (-dt_secs / tau_secs).exp();
    prev + alpha * (sample - prev)
}

pub fn formula(input: &Inputs, cfg: &ScoreConfig) -> (f64, Components) {
    let load = if input.cores == 0 {
        1.0 - clamp01(input.load15)
    } else {
        1.0 - clamp01(input.load1 / input.cores as f64)
    };
    let users = 1.0 - clamp01(input.users / input_soft_cap(cfg));
    let bw_capacity = 1.0 - clamp01(input.tx_bps / input_link_capacity(input));
    let per_user = input.tx_bps / input.users.max(1.0);
    let bw_per_user = clamp01(per_user / input_target(cfg));
    let expected = (86_400.0 / cfg.heartbeat_every_secs.max(1) as f64).max(1.0);
    let reliability = clamp01(input.heartbeat_points_24h as f64 / expected);
    let uptime = clamp01(input.uptime_days.min(30.0) / 30.0);
    let components = Components {
        load,
        users,
        bw_capacity,
        bw_per_user,
        reliability,
        uptime,
    };
    let score = 100.0
        * (cfg.w_load * load
            + cfg.w_users * users
            + cfg.w_bw_capacity * bw_capacity
            + cfg.w_bw_per_user * bw_per_user
            + cfg.w_reliability * reliability
            + cfg.w_uptime * uptime);
    (score.clamp(0.0, 100.0), components)
}

fn input_soft_cap(cfg: &ScoreConfig) -> f64 {
    if cfg.soft_cap > 0.0 {
        cfg.soft_cap
    } else {
        1.0
    }
}

fn input_link_capacity(input: &Inputs) -> f64 {
    if input.link_capacity > 0.0 {
        input.link_capacity
    } else {
        1.0
    }
}

fn input_target(cfg: &ScoreConfig) -> f64 {
    if cfg.target_bps > 0.0 {
        cfg.target_bps
    } else {
        1.0
    }
}

pub fn advance(prev: Option<&Ewma>, raw: f64, now_ms: i64, stale: bool) -> Ewma {
    if stale {
        return Ewma {
            m5: 0.0,
            h1: 0.0,
            h24: 0.0,
            at_ms: now_ms,
        };
    }
    let Some(prev) = prev else {
        return Ewma {
            m5: raw,
            h1: raw,
            h24: raw,
            at_ms: now_ms,
        };
    };
    let dt = ((now_ms - prev.at_ms) as f64 / 1000.0).max(0.0);
    Ewma {
        m5: ewma_step(prev.m5, raw, dt, TAU_5M_SECS),
        h1: ewma_step(prev.h1, raw, dt, TAU_1H_SECS),
        h24: ewma_step(prev.h24, raw, dt, TAU_24H_SECS),
        at_ms: now_ms,
    }
}

pub fn observe(
    storage: &MetricStorage,
    node: &NodeView,
    cfg: &ScoreConfig,
    now_ms: i64,
) -> NodeScore {
    let load1 = storage
        .node_latest_value(&node.id, "sys.loadavg_1")
        .unwrap_or(0.0);
    let load15 = storage
        .node_latest_value(&node.id, "sys.loadavg_15")
        .unwrap_or(0.0);
    let (rx_bps, tx_bps) = iface_bps(storage, node);
    let users = sum_latest(storage, &node.id, "user.traffic.online");
    let last_hb = storage.get_last_heartbeat(&node.id);
    let stale = match last_hb {
        Some(ts) => now_ms.saturating_sub(ts) > cfg.stale_after_secs as i64 * 1000,
        None => true,
    };
    let from = now_ms.saturating_sub(DAY_MS);
    let beats = count_named(storage, &node.id, "sys.heartbeat", from, now_ms);
    let uptime_days = ((now_ms.saturating_sub(node.created_at_ms)) as f64 / DAY_MS as f64).max(0.0);
    let link_capacity = if node.max_bandwidth_bps > 0 {
        node.max_bandwidth_bps as f64
    } else {
        cfg.link_capacity
    };
    let inputs = Inputs {
        load1,
        load15,
        cores: node.cores,
        users,
        tx_bps,
        link_capacity,
        heartbeat_points_24h: beats,
        uptime_days,
    };
    let (raw, components) = formula(&inputs, cfg);
    NodeScore {
        node_id: node.id,
        hostname: node.hostname.clone(),
        label: node.label.clone(),
        score: if stale { 0.0 } else { raw },
        stale,
        tx_bps,
        rx_bps,
        users,
        per_user_bps: tx_bps / users.max(1.0),
        load1,
        components,
        raw,
    }
}

pub fn apply_saved_total(storage: &MetricStorage, row: &mut NodeScore) {
    if row.stale {
        row.score = 0.0;
        return;
    }
    if let Some(saved) = storage.node_latest_value(&row.node_id, SERIES_TOTAL) {
        row.score = saved;
    }
}

pub fn publish(
    storage: &MetricStorage,
    node_id: Uuid,
    row: &NodeScore,
    smooth: &Ewma,
    now_ms: i64,
) {
    let total = if row.stale { 0.0 } else { smooth.m5 };
    let h1 = if row.stale { 0.0 } else { smooth.h1 };
    let h24 = if row.stale { 0.0 } else { smooth.h24 };
    write(storage, node_id, SERIES_TOTAL, total, now_ms);
    write(storage, node_id, SERIES_EWMA_1H, h1, now_ms);
    write(storage, node_id, SERIES_EWMA_24H, h24, now_ms);
    write(storage, node_id, SERIES_LOAD, row.components.load, now_ms);
    write(storage, node_id, SERIES_USERS, row.components.users, now_ms);
    write(
        storage,
        node_id,
        SERIES_BW_CAPACITY,
        row.components.bw_capacity,
        now_ms,
    );
    write(
        storage,
        node_id,
        SERIES_BW_PER_USER,
        row.components.bw_per_user,
        now_ms,
    );
    write(
        storage,
        node_id,
        SERIES_RELIABILITY,
        row.components.reliability,
        now_ms,
    );
    write(storage, node_id, SERIES_UPTIME, row.components.uptime, now_ms);
}

pub fn tick(
    nodes: &[NodeView],
    storage: &MetricStorage,
    cfg: &ScoreConfig,
    smooth: &mut HashMap<Uuid, Ewma>,
    now_ms: i64,
) {
    for node in nodes {
        let row = observe(storage, node, cfg, now_ms);
        let next = advance(smooth.get(&node.id), row.raw, now_ms, row.stale);
        publish(storage, node.id, &row, &next, now_ms);
        smooth.insert(node.id, next);
    }
}

pub fn history(storage: &MetricStorage, node_id: &Uuid, from_ms: i64, to_ms: i64) -> Vec<MetricPoint> {
    points_named(storage, node_id, SERIES_TOTAL, from_ms, to_ms)
}

fn iface_bps(storage: &MetricStorage, node: &NodeView) -> (f64, f64) {
    let rx_name = format!("net.{}.rx_bps", node.interface);
    let tx_name = format!("net.{}.tx_bps", node.interface);
    let rx = storage.node_latest_value(&node.id, &rx_name);
    let tx = storage.node_latest_value(&node.id, &tx_name);
    if rx.is_some() || tx.is_some() {
        return (rx.unwrap_or(0.0), tx.unwrap_or(0.0));
    }
    let (sum_rx, sum_tx) = storage.node_network_traffic(&node.id);
    (sum_rx.unwrap_or(0.0), sum_tx.unwrap_or(0.0))
}

fn sum_latest(storage: &MetricStorage, node_id: &Uuid, metric: &str) -> f64 {
    let Some(node_map) = storage.inner.get(node_id) else {
        return 0.0;
    };
    let mut total = 0.0;
    for entry in node_map.iter() {
        let hash = *entry.key();
        let Some(meta) = storage.metadata.get(&hash) else {
            continue;
        };
        if meta.value().0 == metric {
            if let Some(last) = entry.value().back() {
                total += last.value.max(0.0);
            }
        }
    }
    total
}

fn count_named(storage: &MetricStorage, node_id: &Uuid, metric: &str, from_ms: i64, to_ms: i64) -> u64 {
    points_named(storage, node_id, metric, from_ms, to_ms).len() as u64
}

fn points_named(
    storage: &MetricStorage,
    node_id: &Uuid,
    metric: &str,
    from_ms: i64,
    to_ms: i64,
) -> Vec<MetricPoint> {
    let Some(node_map) = storage.inner.get(node_id) else {
        return Vec::new();
    };
    let mut points = Vec::new();
    for entry in node_map.iter() {
        let hash = *entry.key();
        let Some(meta) = storage.metadata.get(&hash) else {
            continue;
        };
        if meta.value().0 != metric {
            continue;
        }
        points.extend(
            entry
                .value()
                .iter()
                .filter(|p| p.timestamp >= from_ms && p.timestamp <= to_ms)
                .cloned(),
        );
    }
    points.sort_by_key(|p| p.timestamp);
    points
}

fn write(storage: &MetricStorage, node_id: Uuid, name: &str, value: f64, now_ms: i64) {
    storage.insert_envelope(MetricEnvelope {
        node_id,
        name: name.to_string(),
        value,
        timestamp: now_ms,
        tags: std::collections::BTreeMap::new(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use warp::Filter;

    fn cfg() -> ScoreConfig {
        ScoreConfig::default()
    }

    fn ideal_inputs() -> Inputs {
        Inputs {
            load1: 2.0,
            load15: 2.0,
            cores: 4,
            users: 25.0,
            tx_bps: 25_000_000.0,
            link_capacity: 125_000_000.0,
            heartbeat_points_24h: 1440,
            uptime_days: 15.0,
        }
    }

    #[test]
    fn synthetic_metrics_match_formula() {
        let (score, parts) = formula(&ideal_inputs(), &cfg());
        assert!((parts.load - 0.5).abs() < 1e-9);
        assert!((parts.users - 0.75).abs() < 1e-9);
        assert!((parts.bw_capacity - 0.8).abs() < 1e-9);
        assert!((parts.bw_per_user - 1.0).abs() < 1e-9);
        assert!((parts.reliability - 1.0).abs() < 1e-9);
        assert!((parts.uptime - 0.5).abs() < 1e-9);
        assert!((score - 74.5).abs() < 1e-9);
    }

    #[test]
    fn missing_cores_uses_load15() {
        let mut input = ideal_inputs();
        input.cores = 0;
        input.load1 = 99.0;
        input.load15 = 0.4;
        let (_, parts) = formula(&input, &cfg());
        assert!((parts.load - 0.6).abs() < 1e-9);
    }

    #[test]
    fn silent_node_is_stale_zero() {
        let storage = MetricStorage::new(100, 86_400 * 8);
        let now = 1_700_000_000_000;
        let node = Uuid::new_v4();
        storage.insert_envelope(MetricEnvelope {
            node_id: node,
            name: "sys.heartbeat".into(),
            value: 1.0,
            timestamp: now - 11 * 60 * 1000,
            tags: std::collections::BTreeMap::new(),
        });
        let view = NodeView {
            id: node,
            hostname: "silent".into(),
            label: "s".into(),
            cores: 4,
            interface: "eth0".into(),
            max_bandwidth_bps: 0,
            created_at_ms: now - 15 * DAY_MS,
        };
        let row = observe(&storage, &view, &cfg(), now);
        assert!(row.stale);
        assert_eq!(row.score, 0.0);
    }

    #[test]
    fn ewma_damps_a_spike() {
        let stepped = ewma_step(0.0, 100.0, 60.0, TAU_5M_SECS);
        let expected = 100.0 * (1.0 - (-0.2_f64).exp());
        assert!((stepped - expected).abs() < 1e-9);
        assert!(stepped < 20.0);

        let mut smooth = HashMap::new();
        let first = advance(None, 10.0, 1_000, false);
        smooth.insert(Uuid::nil(), first);
        let next = advance(smooth.get(&Uuid::nil()), 100.0, 1_000 + 60_000, false);
        assert!((next.m5 - (10.0 + (100.0 - 10.0) * (1.0 - (-0.2_f64).exp()))).abs() < 1e-6);
        assert!(next.h1 - 10.0 < next.m5 - 10.0);
        assert!(next.h24 - 10.0 < next.h1 - 10.0);
    }

    #[test]
    fn stale_resets_ewma() {
        let prev = Ewma {
            m5: 80.0,
            h1: 70.0,
            h24: 60.0,
            at_ms: 0,
        };
        let next = advance(Some(&prev), 90.0, 60_000, true);
        assert_eq!(next.m5, 0.0);
        assert_eq!(next.h1, 0.0);
        assert_eq!(next.h24, 0.0);
    }

    #[test]
    fn config_defaults_roundtrip() {
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(default)]
            score: ScoreConfig,
        }
        let parsed: Wrap = toml::from_str("").unwrap();
        assert_eq!(parsed.score, ScoreConfig::default());
        assert_eq!(parsed.score.interval_secs, 60);
        assert_eq!(parsed.score.stale_after_secs, 600);
        assert_eq!(parsed.score.target_bps, 625_000.0);
        assert_eq!(parsed.score.soft_cap, 100.0);
        let partial: ScoreConfig = toml::from_str("interval_secs = 30\n").unwrap();
        assert_eq!(partial.interval_secs, 30);
        assert_eq!(partial.target_bps, 625_000.0);
        assert_eq!(partial.w_load, 0.25);
        let again: ScoreConfig = toml::from_str(&toml::to_string(&parsed.score).unwrap()).unwrap();
        assert_eq!(again, parsed.score);
    }

    fn seed_live(storage: &MetricStorage, node: Uuid, now: i64) {
        let tags = std::collections::BTreeMap::new();
        let beat_every = 60_000;
        let beats = 1440;
        for i in 0..beats {
            storage.insert_envelope(MetricEnvelope {
                node_id: node,
                name: "sys.heartbeat".into(),
                value: 1.0,
                timestamp: now - (beats - 1 - i) as i64 * beat_every,
                tags: tags.clone(),
            });
        }
        storage.insert_envelope(MetricEnvelope {
            node_id: node,
            name: "sys.loadavg_1".into(),
            value: 2.0,
            timestamp: now,
            tags: tags.clone(),
        });
        storage.insert_envelope(MetricEnvelope {
            node_id: node,
            name: "sys.loadavg_15".into(),
            value: 2.0,
            timestamp: now,
            tags: tags.clone(),
        });
        storage.insert_envelope(MetricEnvelope {
            node_id: node,
            name: "net.eth0.tx_bps".into(),
            value: 25_000_000.0,
            timestamp: now,
            tags: tags.clone(),
        });
        storage.insert_envelope(MetricEnvelope {
            node_id: node,
            name: "net.eth0.rx_bps".into(),
            value: 10_000_000.0,
            timestamp: now,
            tags: tags.clone(),
        });
        for n in 0..25 {
            let mut peer_tags = tags.clone();
            peer_tags.insert("conn_id".into(), format!("peer-{n}"));
            storage.insert_envelope(MetricEnvelope {
                node_id: node,
                name: "user.traffic.online".into(),
                value: 1.0,
                timestamp: now,
                tags: peer_tags,
            });
        }
    }

    #[tokio::test]
    async fn http_score_and_history_payloads() {
        let storage = Arc::new(MetricStorage::new(20_000, 86_400 * 8));
        let now = 1_700_000_000_000;
        let node = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        seed_live(&storage, node, now);
        let view = NodeView {
            id: node,
            hostname: "ams-1".into(),
            label: "ams".into(),
            cores: 4,
            interface: "eth0".into(),
            max_bandwidth_bps: 0,
            created_at_ms: now - 15 * DAY_MS,
        };
        let cfg = cfg();
        let mut smooth = HashMap::new();
        tick(std::slice::from_ref(&view), &storage, &cfg, &mut smooth, now);
        tick(
            std::slice::from_ref(&view),
            &storage,
            &cfg,
            &mut smooth,
            now + 60_000,
        );

        let mut row = observe(&storage, &view, &cfg, now + 60_000);
        apply_saved_total(&storage, &mut row);
        let list = serde_json::json!({ "nodes": [row] });
        let points = history(&storage, &node, now, now + 60_000);
        let hist = serde_json::json!({
            "node_id": node,
            "metric": SERIES_TOTAL,
            "points": points,
        });

        let list_body = serde_json::to_string(&list).unwrap();
        let hist_body = serde_json::to_string(&hist).unwrap();
        let list_body_2 = list_body.clone();
        let hist_body_2 = hist_body.clone();

        let list_filter = warp::path!("v1" / "admin" / "nodes" / "score").map(move || list_body.clone());
        let hist_filter =
            warp::path!("v1" / "admin" / "nodes" / "score" / "history").map(move || hist_body.clone());

        let list_res = warp::test::request()
            .method("GET")
            .path("/v1/admin/nodes/score")
            .reply(&list_filter)
            .await;
        let hist_res = warp::test::request()
            .method("GET")
            .path("/v1/admin/nodes/score/history?node=11111111-2222-3333-4444-555555555555&from=1700000000000&to=1700000060000")
            .reply(&hist_filter)
            .await;

        assert_eq!(list_res.status(), 200);
        assert_eq!(hist_res.status(), 200);
        assert_eq!(list_res.body().as_ref(), list_body_2.as_bytes());
        assert_eq!(hist_res.body().as_ref(), hist_body_2.as_bytes());
        println!("SCORE {}", list_body_2);
        println!("HISTORY {}", hist_body_2);

        let parsed: serde_json::Value = serde_json::from_slice(list_res.body()).unwrap();
        assert!(parsed["nodes"][0]["stale"].as_bool() == Some(false));
        assert!((parsed["nodes"][0]["score"].as_f64().unwrap() - 74.5).abs() < 1.0);
        assert_eq!(parsed["nodes"][0]["users"].as_f64().unwrap(), 25.0);
        let hist_parsed: serde_json::Value = serde_json::from_slice(hist_res.body()).unwrap();
        assert_eq!(hist_parsed["points"].as_array().unwrap().len(), 2);
    }
}
