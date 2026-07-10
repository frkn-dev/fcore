use crate::aggregator::{Aggregator, MetricSample};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::Mutex;
use warp::Filter;

pub async fn start_prometheus_server(
    listen: String,
    port: u16,
    aggregator: Arc<Mutex<Aggregator>>,
) {
    let route = warp::path("metrics")
        .and(warp::get())
        .and(warp::path::end())
        .and(with_aggregator(aggregator))
        .and_then(metrics_handler);

    let addr: std::net::SocketAddr = format!("{}:{}", listen, port)
        .parse()
        .expect("Invalid prometheus listen address");

    tracing::info!("Prometheus server listening on http://{}/metrics", addr);
    warp::serve(route).run(addr).await;
}

fn with_aggregator(
    aggregator: Arc<Mutex<Aggregator>>,
) -> impl Filter<Extract = (Arc<Mutex<Aggregator>>,), Error = Infallible> + Clone {
    warp::any().map(move || aggregator.clone())
}

async fn metrics_handler(aggregator: Arc<Mutex<Aggregator>>) -> Result<impl warp::Reply, Infallible> {
    let samples = {
        let mut agg = aggregator.lock().await;
        agg.flush(u64::MAX)
    };

    let body = render_prometheus(&samples);
    Ok(warp::reply::with_header(body, "Content-Type", "text/plain; charset=utf-8"))
}

fn render_prometheus(samples: &[MetricSample]) -> String {
    let mut lines: Vec<String> = Vec::new();

    for sample in samples {
        let name = sanitize_name(&sample.name);
        let labels = format_labels(&sample.tags);
        lines.push(format!("{}{} {}", name, labels, sample.value));
    }

    if lines.is_empty() {
        lines.push("# no pixel metrics yet".to_string());
    }

    lines.join("\n") + "\n"
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
