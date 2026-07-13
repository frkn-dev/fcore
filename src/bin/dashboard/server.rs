use crate::config::Config;
use chrono::Datelike;
use serde::Serialize;
use std::convert::Infallible;
use std::sync::Arc;
use warp::Filter;

const ADMIN_HTML: &str = include_str!("admin.html");

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub http: reqwest::Client,
}

#[derive(Serialize)]
pub struct OverviewResponse {
    pub visits: u64,
    pub payments: u64,
    pub revenue: f64,
    pub trials: u64,
    pub conversions: u64,
}

#[derive(serde::Deserialize, Debug)]
pub struct PeriodQuery {
    #[serde(default = "default_overview_period")]
    pub period: String,
}

#[derive(serde::Deserialize, Debug)]
pub struct OverviewQuery {
    #[serde(default = "default_overview_period")]
    pub period: String,
}

fn default_overview_period() -> String {
    "today".to_string()
}

#[derive(Clone, Copy, Debug)]
enum OverviewPeriod {
    Today,
    Yesterday,
    Last24h,
    Last7d,
    Last30d,
    Last90d,
    ThisMonth,
}

impl OverviewPeriod {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "today" => Some(Self::Today),
            "yesterday" => Some(Self::Yesterday),
            "24h" => Some(Self::Last24h),
            "7d" => Some(Self::Last7d),
            "30d" => Some(Self::Last30d),
            "90d" => Some(Self::Last90d),
            "month" => Some(Self::ThisMonth),
            _ => s.parse::<i64>().ok().and_then(|_| Some(Self::Last24h)),
        }
    }

    /// Returns (from_ms, to_ms, api_period_days) for backend queries.
    fn bounds(&self) -> (i64, i64, i64) {
        let now = chrono::Utc::now();
        let to_ms = now.timestamp_millis();

        match self {
            Self::Today => {
                let start = now.date_naive().and_hms_opt(0, 0, 0).unwrap();
                let from_ms = start.and_utc().timestamp_millis();
                (from_ms, to_ms, 2)
            }
            Self::Yesterday => {
                let yesterday = now.date_naive() - chrono::Days::new(1);
                let start = yesterday.and_hms_opt(0, 0, 0).unwrap();
                let end = yesterday.and_hms_opt(23, 59, 59).unwrap();
                let from_ms = start.and_utc().timestamp_millis();
                let to_ms = end.and_utc().timestamp_millis();
                (from_ms, to_ms, 2)
            }
            Self::Last24h => {
                let from_ms = to_ms - 24 * 60 * 60 * 1000;
                (from_ms, to_ms, 2)
            }
            Self::Last7d => {
                let from_ms = to_ms - 7 * 24 * 60 * 60 * 1000;
                (from_ms, to_ms, 7)
            }
            Self::Last30d => {
                let from_ms = to_ms - 30 * 24 * 60 * 60 * 1000;
                (from_ms, to_ms, 30)
            }
            Self::Last90d => {
                let from_ms = to_ms - 90 * 24 * 60 * 60 * 1000;
                (from_ms, to_ms, 90)
            }
            Self::ThisMonth => {
                let start = now.date_naive().with_day(1).unwrap().and_hms_opt(0, 0, 0).unwrap();
                let from_ms = start.and_utc().timestamp_millis();
                (from_ms, to_ms, 31)
            }
        }
    }
}

pub async fn start_server(config: Config) {
    let state = Arc::new(AppState {
        config,
        http: reqwest::Client::new(),
    });

    let index = warp::path::end()
        .and(warp::get())
        .map(|| warp::reply::html(ADMIN_HTML));

    let api_overview = warp::path("api")
        .and(warp::path("overview"))
        .and(warp::get())
        .and(warp::path::end())
        .and(warp::query::<OverviewQuery>())
        .and(with_state(state.clone()))
        .and_then(overview_handler);

    let api_sales = warp::path("api")
        .and(warp::path("sales"))
        .and(warp::get())
        .and(warp::path::end())
        .and(warp::query::<PeriodQuery>())
        .and(with_state(state.clone()))
        .and_then(sales_proxy_handler);

    let api_pixel = warp::path("api")
        .and(warp::path("pixel"))
        .and(warp::get())
        .and(warp::path::end())
        .and(warp::query::<PeriodQuery>())
        .and(with_state(state.clone()))
        .and_then(pixel_proxy_handler);

    let health = warp::path("health")
        .and(warp::get())
        .and(warp::path::end())
        .map(|| "ok");

    let routes = index
        .or(api_overview)
        .or(api_sales)
        .or(api_pixel)
        .or(health);

    let cors = warp::cors()
        .allow_methods(vec!["GET", "POST", "OPTIONS"])
        .allow_headers(vec!["Content-Type", "Authorization"])
        .allow_any_origin();

    let routes = routes.with(cors);

    let addr: std::net::SocketAddr = format!("{}:{}", state.config.dashboard.listen, state.config.dashboard.port)
        .parse()
        .expect("Invalid dashboard listen address");

    tracing::info!("Business dashboard listening on http://{}", addr);
    warp::serve(routes).run(addr).await;
}

fn with_state(
    state: Arc<AppState>,
) -> impl Filter<Extract = (Arc<AppState>,), Error = Infallible> + Clone {
    warp::any().map(move || state.clone())
}

async fn overview_handler(
    query: OverviewQuery,
    state: Arc<AppState>,
) -> Result<impl warp::Reply, Infallible> {
    let period = OverviewPeriod::from_str(&query.period).unwrap_or(OverviewPeriod::Last24h);
    let overview = build_overview(state, period).await.unwrap_or(OverviewResponse {
        visits: 0,
        payments: 0,
        revenue: 0.0,
        trials: 0,
        conversions: 0,
    });
    Ok(warp::reply::json(&overview))
}

async fn build_overview(
    state: Arc<AppState>,
    period: OverviewPeriod,
) -> Result<OverviewResponse, reqwest::Error> {
    let (from_ms, to_ms, api_period) = period.bounds();

    let pixel_url = format!(
        "{}/api/metrics?from_ms={}&to_ms={}",
        state.config.pixel.endpoint, from_ms, to_ms
    );
    let mut pixel_req = state.http.get(&pixel_url);
    if let Some(token) = &state.config.pixel.token {
        pixel_req = pixel_req.header("Authorization", format!("Bearer {}", token));
    }
    let visits = match pixel_req.send().await {
        Ok(resp) => {
            let data: serde_json::Value = resp.json().await.unwrap_or_default();
            count_visits(&data)
        }
        Err(err) => {
            tracing::warn!("Failed to fetch pixel metrics: {}", err);
            0
        }
    };

    let payment_url = format!(
        "{}/analytics/sales?period={}&granularity=daily",
        state.config.payment.endpoint, api_period
    );
    let payment_req = state
        .http
        .get(&payment_url)
        .header(
            "Authorization",
            format!("Bearer {}", state.config.payment.analytics_token),
        );
    let (payments, revenue) = match payment_req.send().await {
        Ok(resp) => {
            let data: serde_json::Value = resp.json().await.unwrap_or_default();
            extract_sales_in_range(&data, from_ms, to_ms)
        }
        Err(err) => {
            tracing::warn!("Failed to fetch payment sales: {}", err);
            (0, 0.0)
        }
    };

    let trials_url = format!(
        "{}/analytics/trials?period={}&granularity=daily",
        state.config.mrkting.endpoint, api_period
    );
    let mut trials_req = state.http.get(&trials_url);
    if let Some(token) = &state.config.mrkting.token {
        trials_req = trials_req.header("Authorization", format!("Bearer {}", token));
    }
    let trials = match trials_req.send().await {
        Ok(resp) => {
            let data: serde_json::Value = resp.json().await.unwrap_or_default();
            extract_trials_in_range(&data, from_ms, to_ms)
        }
        Err(err) => {
            tracing::warn!("Failed to fetch trials: {}", err);
            0
        }
    };

    let conversions_url = format!(
        "{}/analytics/conversions?period={}&granularity=daily",
        state.config.mrkting.endpoint, api_period
    );
    let mut conversions_req = state.http.get(&conversions_url);
    if let Some(token) = &state.config.mrkting.token {
        conversions_req = conversions_req.header("Authorization", format!("Bearer {}", token));
    }
    let conversions = match conversions_req.send().await {
        Ok(resp) => {
            let data: serde_json::Value = resp.json().await.unwrap_or_default();
            extract_conversions_in_range(&data, from_ms, to_ms)
        }
        Err(err) => {
            tracing::warn!("Failed to fetch conversions: {}", err);
            0
        }
    };

    Ok(OverviewResponse {
        visits,
        payments,
        revenue,
        trials,
        conversions,
    })
}

fn extract_conversions_in_range(data: &serde_json::Value, from_ms: i64, to_ms: i64) -> u64 {
    let mut total = 0u64;
    if let Some(data_arr) = data.get("data").and_then(|d| d.as_array()) {
        for bucket in data_arr {
            if let Some(bucket_str) = bucket.get("bucket").and_then(|b| b.as_str()) {
                if let Ok(bucket_ms) = parse_bucket_ms(bucket_str) {
                    if bucket_ms >= from_ms && bucket_ms <= to_ms {
                        total += bucket
                            .get("conversions")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                    }
                }
            }
        }
    }
    total
}

fn count_visits(data: &serde_json::Value) -> u64 {
    let mut total = 0u64;
    if let Some(series) = data.get("series").and_then(|s| s.as_array()) {
        for s in series {
            if s.get("metric").and_then(|m| m.as_str()) == Some("web.visits.total") {
                if let Some(points) = s.get("points").and_then(|p| p.as_array()) {
                    for p in points {
                        if let Some(y) = p.get("y").and_then(|v| v.as_f64()) {
                            total += y as u64;
                        } else if let Some(y) = p.get("value").and_then(|v| v.as_f64()) {
                            total += y as u64;
                        }
                    }
                }
            }
        }
    }
    total
}

fn extract_sales_in_range(data: &serde_json::Value, from_ms: i64, to_ms: i64) -> (u64, f64) {
    let mut payments = 0u64;
    let mut revenue = 0.0;
    if let Some(data_arr) = data.get("data").and_then(|d| d.as_array()) {
        for bucket in data_arr {
            if let Some(bucket_str) = bucket.get("bucket").and_then(|b| b.as_str()) {
                if let Ok(bucket_ms) = parse_bucket_ms(bucket_str) {
                    if bucket_ms >= from_ms && bucket_ms <= to_ms {
                        payments += bucket
                            .get("confirmed")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        revenue += bucket
                            .get("revenue")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                    }
                }
            }
        }
    }
    (payments, revenue)
}

fn extract_trials_in_range(data: &serde_json::Value, from_ms: i64, to_ms: i64) -> u64 {
    let mut total = 0u64;
    if let Some(data_arr) = data.get("data").and_then(|d| d.as_array()) {
        for bucket in data_arr {
            if let Some(bucket_str) = bucket.get("bucket").and_then(|b| b.as_str()) {
                if let Ok(bucket_ms) = parse_bucket_ms(bucket_str) {
                    if bucket_ms >= from_ms && bucket_ms <= to_ms {
                        total += bucket
                            .get("trials")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                    }
                }
            }
        }
    }
    total
}

fn parse_bucket_ms(bucket: &str) -> Result<i64, chrono::ParseError> {
    let dt = if bucket.len() == 7 {
        chrono::NaiveDate::parse_from_str(bucket, "%Y-%m")?
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
    } else {
        chrono::NaiveDate::parse_from_str(bucket, "%Y-%m-%d")?
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
    };
    Ok(dt.timestamp_millis())
}

async fn sales_proxy_handler(
    query: PeriodQuery,
    state: Arc<AppState>,
) -> Result<impl warp::Reply, Infallible> {
    let period = OverviewPeriod::from_str(&query.period).unwrap_or(OverviewPeriod::Last24h);
    let (from_ms, to_ms, api_period) = period.bounds();

    let url = format!(
        "{}/analytics/sales?period={}&granularity=daily",
        state.config.payment.endpoint, api_period
    );

    let resp = state
        .http
        .get(&url)
        .header(
            "Authorization",
            format!("Bearer {}", state.config.payment.analytics_token),
        )
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = warp::http::StatusCode::from_u16(r.status().as_u16())
                .unwrap_or(warp::http::StatusCode::OK);
            let mut body: serde_json::Value = r.json().await.unwrap_or_default();
            if let Some(data) = body.get_mut("data").and_then(|d| d.as_array_mut()) {
                data.retain(|bucket| {
                    bucket
                        .get("bucket")
                        .and_then(|b| b.as_str())
                        .and_then(|b| parse_bucket_ms(b).ok())
                        .map(|ms| ms >= from_ms && ms <= to_ms)
                        .unwrap_or(false)
                });
            }
            Ok(warp::reply::with_status(
                warp::reply::json(&body),
                status,
            ))
        }
        Err(err) => {
            tracing::error!("Payment sales proxy error: {}", err);
            Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": err.to_string()})),
                warp::http::StatusCode::BAD_GATEWAY,
            ))
        }
    }
}

async fn pixel_proxy_handler(
    query: PeriodQuery,
    state: Arc<AppState>,
) -> Result<impl warp::Reply, Infallible> {
    let period = OverviewPeriod::from_str(&query.period).unwrap_or(OverviewPeriod::Last24h);
    let (from_ms, to_ms, _) = period.bounds();

    let url = format!(
        "{}/api/metrics?from_ms={}&to_ms={}",
        state.config.pixel.endpoint, from_ms, to_ms
    );

    let mut req = state.http.get(&url);
    if let Some(token) = &state.config.pixel.token {
        req = req.header("Authorization", format!("Bearer {}", token));
    }

    match req.send().await {
        Ok(r) => {
            let status = warp::http::StatusCode::from_u16(r.status().as_u16())
                .unwrap_or(warp::http::StatusCode::OK);
            let body: Vec<u8> = r.bytes().await.unwrap_or_default().to_vec();
            Ok(warp::reply::with_status(body, status))
        }
        Err(err) => {
            tracing::error!("Pixel metrics proxy error: {}", err);
            Ok(warp::reply::with_status(
                format!("{{\"error\":\"{}\"}}", err).into_bytes(),
                warp::http::StatusCode::BAD_GATEWAY,
            ))
        }
    }
}
