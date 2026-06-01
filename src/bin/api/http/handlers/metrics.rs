use futures::SinkExt;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;

use fcore::MetricStorage;

#[derive(Debug, Deserialize, Clone)]
pub struct WsMetricQuery {
    pub metric: String,

    #[serde(flatten)]
    pub tags: BTreeMap<String, String>,

    pub from: Option<i64>,
    pub mode: Option<String>,
    pub group_by: Option<String>,
}

pub async fn metrics_ws_handler(
    socket: warp::ws::WebSocket,
    query: WsMetricQuery,
    storage: Arc<MetricStorage>,
) {
    let (mut tx, _) = socket.split();
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));

    let metric = query.metric.clone();
    let tags = query.tags.clone();
    let from_base = query.from;
    let mode = query.mode.clone().unwrap_or_else(|| "range".to_string());
    let group_by = query.group_by.clone();

    loop {
        ticker.tick().await;
        let now = chrono::Utc::now().timestamp_millis();
        let from = from_base.unwrap_or(now - 600_000);
        let hashes = storage.series_for(Some(&metric), &tags);

        let msg = match mode.as_str() {
            "aggregated" => {
                let mut time_map: BTreeMap<i64, (f64, usize)> = BTreeMap::new();
                for node in storage.inner.iter() {
                    for hash in &hashes {
                        let points = storage.get_range(node.key(), *hash, from, now);
                        for p in points {
                            let entry = time_map.entry(p.timestamp).or_insert((0.0, 0));
                            entry.0 += p.value;
                            entry.1 += 1;
                        }
                    }
                }
                let aggregated: Vec<(i64, f64)> = time_map
                    .into_iter()
                    .map(|(ts, (sum, cnt))| (ts, sum / cnt as f64))
                    .collect();
                serde_json::json!({
                    "type": "aggregated",
                    "metric": metric,
                    "data": aggregated
                })
            }
            _ => {
                if let Some(group_by_tag) = &group_by {
                    if group_by_tag == "node" {
                        let mut by_node: BTreeMap<String, Vec<(i64, f64)>> = BTreeMap::new();
                        for node in storage.inner.iter() {
                            let node_id = node.key().to_string();
                            let mut points = Vec::new();
                            for hash in &hashes {
                                let pts = storage.get_range(node.key(), *hash, from, now);
                                points.extend(pts.into_iter().map(|p| (p.timestamp, p.value)));
                            }
                            if points.is_empty() {
                                continue;
                            }
                            points.sort_by_key(|(ts, _)| *ts);
                            by_node.entry(node_id).or_default().extend(points);
                        }
                        serde_json::json!({
                            "type": "multiline",
                            "metric": metric,
                            "group_by": "node",
                            "data": by_node
                        })
                    } else {
                        let mut groups: BTreeMap<String, Vec<(i64, f64)>> = BTreeMap::new();
                        for node in storage.inner.iter() {
                            let node_id = *node.key();
                            for hash in &hashes {
                                let meta = match storage.metadata.get(hash) {
                                    Some(m) => m,
                                    None => continue,
                                };
                                let series_tags = &meta.value().1;
                                let group_value = series_tags
                                    .get(group_by_tag)
                                    .map(|v| v.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let points = storage.get_range(&node_id, *hash, from, now);
                                let ts_points: Vec<(i64, f64)> =
                                    points.into_iter().map(|p| (p.timestamp, p.value)).collect();
                                groups.entry(group_value).or_default().extend(ts_points);
                            }
                        }
                        for points in groups.values_mut() {
                            points.sort_by_key(|(ts, _)| *ts);
                        }
                        serde_json::json!({
                            "type": "multiline",
                            "metric": metric,
                            "group_by": group_by_tag,
                            "data": groups
                        })
                    }
                } else {
                    let mut result = Vec::new();
                    for node in storage.inner.iter() {
                        for hash in &hashes {
                            let points = storage.get_range(node.key(), *hash, from, now);
                            result.extend(points.into_iter().map(|p| (p.timestamp, p.value)));
                        }
                    }
                    result.sort_by_key(|p| p.0);
                    serde_json::json!({
                        "type": "range",
                        "metric": metric,
                        "data": result
                    })
                }
            }
        };

        if tx
            .send(warp::ws::Message::text(msg.to_string()))
            .await
            .is_err()
        {
            break;
        }
    }
}
