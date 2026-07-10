use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

use fcore::WebMetricStorage;

use crate::http::handlers::admin::{check_token, not_found, unauthorized};

#[derive(Deserialize)]
pub struct WebMetricsIngestRequest {
    pub samples: Vec<WebMetricSampleIn>,
}

#[derive(Deserialize)]
pub struct WebMetricSampleIn {
    pub name: String,
    pub tags: BTreeMap<String, String>,
    pub value: f64,
    pub timestamp_ms: i64,
}

#[derive(Serialize)]
pub struct WebMetricsIngestResponse {
    pub ingested: usize,
}

pub async fn ingest_web_metrics(
    req: WebMetricsIngestRequest,
    storage: Arc<WebMetricStorage>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let count = req.samples.len();
    for sample in req.samples {
        storage.insert(sample.name, sample.tags, sample.value, sample.timestamp_ms);
    }
    Ok(warp::reply::json(&WebMetricsIngestResponse { ingested: count }))
}

#[derive(Debug, Deserialize)]
pub struct WebMetricsQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
}

#[derive(Serialize)]
pub struct WebMetricsResponse {
    pub total_visits: f64,
    pub visits_by_page: Vec<MetricSeries>,
    pub visits_by_country: Vec<MetricSeries>,
    pub visits_by_referer: Vec<MetricSeries>,
    pub timeline: Vec<fcore::MetricPoint>,
}

#[derive(Serialize)]
pub struct MetricSeries {
    pub label: String,
    pub value: f64,
}

pub async fn admin_api_web_metrics_handler(
    query: WebMetricsQuery,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
    storage: Arc<WebMetricStorage>,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection> {
    if !admin_enabled {
        return Ok(not_found());
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(unauthorized());
    }

    let now = chrono::Utc::now().timestamp_millis();
    let from = query.from.unwrap_or(now - 24 * 60 * 60 * 1000);
    let to = query.to.unwrap_or(now);

    let total_visits = storage.query_sum("web.visits.total", &BTreeMap::new(), from, to);

    let visits_by_page = storage
        .query_top("web.visits.page", "page", from, to, 20)
        .into_iter()
        .map(|(label, value)| MetricSeries { label, value })
        .collect();

    let visits_by_country = storage
        .query_top("web.visits.country", "country", from, to, 20)
        .into_iter()
        .map(|(label, value)| MetricSeries { label, value })
        .collect();

    let visits_by_referer = storage
        .query_top("web.visits.referer_domain", "referer_domain", from, to, 20)
        .into_iter()
        .map(|(label, value)| MetricSeries { label, value })
        .collect();

    let timeline = storage.query_points("web.visits.total", &BTreeMap::new(), from, to);

    Ok(Box::new(warp::reply::json(&WebMetricsResponse {
        total_visits,
        visits_by_page,
        visits_by_country,
        visits_by_referer,
        timeline,
    })))
}

#[derive(Debug, Deserialize)]
pub struct WebMetricsTimelineQuery {
    pub metric: String,
    pub from: Option<i64>,
    pub to: Option<i64>,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

#[derive(Serialize)]
pub struct WebMetricsTimelineResponse {
    pub metric: String,
    pub tags: BTreeMap<String, String>,
    pub points: Vec<fcore::MetricPoint>,
}

pub async fn admin_api_web_metrics_timeline_handler(
    query: WebMetricsTimelineQuery,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
    storage: Arc<WebMetricStorage>,
) -> Result<Box<dyn warp::Reply + Send>, warp::Rejection> {
    if !admin_enabled {
        return Ok(not_found());
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(unauthorized());
    }

    let now = chrono::Utc::now().timestamp_millis();
    let from = query.from.unwrap_or(now - 24 * 60 * 60 * 1000);
    let to = query.to.unwrap_or(now);

    let points = storage.query_points(&query.metric, &query.tags, from, to);

    Ok(Box::new(warp::reply::json(&WebMetricsTimelineResponse {
        metric: query.metric,
        tags: query.tags,
        points,
    })))
}
