use crate::aggregator::Aggregator;
use crate::web_storage::WebMetricStorage;
use fcore::MetricPoint;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::Mutex;
use warp::Filter;

const ADMIN_HTML: &str = include_str!("admin.html");

#[derive(serde::Serialize)]
struct SeriesResponse {
    metric: String,
    tags: BTreeMap<String, String>,
    points: Vec<MetricPoint>,
}

#[derive(serde::Serialize)]
struct MetricsResponse {
    from_ms: i64,
    to_ms: i64,
    series: Vec<SeriesResponse>,
}

pub async fn start_admin_server(
    listen: String,
    port: u16,
    cors_origins: Vec<String>,
    aggregator: Arc<Mutex<Aggregator>>,
    storage: Arc<WebMetricStorage>,
) {
    let index = warp::path::end()
        .and(warp::get())
        .map(|| warp::reply::html(ADMIN_HTML));

    let metrics_api = warp::path("api")
        .and(warp::path("metrics"))
        .and(warp::get())
        .and(warp::path::end())
        .and(warp::query::<QueryParams>())
        .and(with_storage(storage.clone()))
        .and_then(metrics_api_handler);

    let prometheus = warp::path("metrics")
        .and(warp::get())
        .and(warp::path::end())
        .and(with_storage(storage.clone()))
        .and_then(prometheus_handler);

    let health = warp::path("health")
        .and(warp::get())
        .and(warp::path::end())
        .map(|| "ok");

    let routes = index
        .or(metrics_api)
        .or(prometheus)
        .or(health);

    let mut cors_builder = warp::cors()
        .allow_methods(vec!["GET", "POST", "OPTIONS"])
        .allow_headers(vec!["Content-Type", "Authorization"])
        .allow_credentials(true)
        .max_age(86400);

    for origin in &cors_origins {
        cors_builder = cors_builder.allow_origin(origin.as_str());
    }

    let routes = routes.with(cors_builder.build());

    let addr: std::net::SocketAddr = format!("{}:{}", listen, port)
        .parse()
        .expect("Invalid admin listen address");

    tracing::info!("Pixel admin server listening on http://{}", addr);
    warp::serve(routes).run(addr).await;

    let _ = aggregator;
}

#[derive(serde::Deserialize)]
struct QueryParams {
    #[serde(default)]
    from_ms: i64,
    #[serde(default)]
    to_ms: i64,
}

fn with_storage(
    storage: Arc<WebMetricStorage>,
) -> impl Filter<Extract = (Arc<WebMetricStorage>,), Error = Infallible> + Clone {
    warp::any().map(move || storage.clone())
}

async fn metrics_api_handler(
    params: QueryParams,
    storage: Arc<WebMetricStorage>,
) -> Result<impl warp::Reply, Infallible> {
    let now = chrono::Utc::now().timestamp_millis();
    let from_ms = if params.from_ms > 0 { params.from_ms } else { now - 24 * 60 * 60 * 1000 };
    let to_ms = if params.to_ms > 0 { params.to_ms } else { now };

    let mut series = Vec::new();
    for metric_map in storage.inner.iter() {
        let metric = metric_map.key().clone();
        for entry in metric_map.iter() {
            let hash = *entry.key();
            let tags = match storage.metadata.get(&hash) {
                Some(t) => t.clone(),
                None => continue,
            };
            let points: Vec<MetricPoint> = entry
                .value()
                .iter()
                .filter(|p| p.timestamp >= from_ms && p.timestamp <= to_ms)
                .cloned()
                .collect();
            if points.is_empty() {
                continue;
            }
            series.push(SeriesResponse {
                metric: metric.clone(),
                tags,
                points,
            });
        }
    }

    let response = MetricsResponse {
        from_ms,
        to_ms,
        series,
    };

    Ok(warp::reply::json(&response))
}

async fn prometheus_handler(storage: Arc<WebMetricStorage>) -> Result<impl warp::Reply, Infallible> {
    let now = chrono::Utc::now().timestamp_millis();
    let from_ms = now - 24 * 60 * 60 * 1000;
    let to_ms = now;

    let mut lines: Vec<String> = Vec::new();
    for metric_map in storage.inner.iter() {
        let metric = metric_map.key();
        for entry in metric_map.iter() {
            let hash = *entry.key();
            let tags = match storage.metadata.get(&hash) {
                Some(t) => t.clone(),
                None => continue,
            };
            let value: f64 = entry
                .value()
                .iter()
                .filter(|p| p.timestamp >= from_ms && p.timestamp <= to_ms)
                .map(|p| p.value)
                .sum();
            if value == 0.0 {
                continue;
            }
            let name = sanitize_name(metric);
            let labels = format_labels(&tags);
            lines.push(format!("{}{} {}", name, labels, value));
        }
    }

    if lines.is_empty() {
        lines.push("# no pixel metrics yet".to_string());
    }

    let body = lines.join("\n") + "\n";
    Ok(warp::reply::with_header(body, "Content-Type", "text/plain; charset=utf-8"))
}

fn sanitize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_alphanumeric() || c == '_' || c == ':' {
            out.push(c);
        } else {
            out.push('_');
        }
        if i == 0 && c.is_ascii_digit() {
            out.insert(0, '_');
        }
    }
    if out.is_empty() {
        out.push_str("metric");
    }
    out
}

fn format_labels(tags: &std::collections::BTreeMap<String, String>) -> String {
    if tags.is_empty() {
        return String::new();
    }

    let parts: Vec<String> = tags
        .iter()
        .map(|(k, v)| format!("{}=\"{}\"", sanitize_name(k), escape_label(v)))
        .collect();
    format!("{{{}}}", parts.join(","))
}

fn escape_label(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}
