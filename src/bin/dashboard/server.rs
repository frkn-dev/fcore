use crate::config::Config;
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
    pub visits_24h: u64,
    pub payments_24h: u64,
    pub revenue_24h: f64,
    pub trials_24h: u64,
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
        .and(with_state(state.clone()))
        .and_then(overview_handler);

    let api_sales = warp::path("api")
        .and(warp::path("sales"))
        .and(warp::get())
        .and(warp::path::end())
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(with_state(state.clone()))
        .and_then(sales_proxy_handler);

    let api_pixel = warp::path("api")
        .and(warp::path("pixel"))
        .and(warp::get())
        .and(warp::path::end())
        .and(warp::query::<std::collections::HashMap<String, String>>())
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

async fn overview_handler(state: Arc<AppState>) -> Result<impl warp::Reply, Infallible> {
    let overview = build_overview(state).await.unwrap_or(OverviewResponse {
        visits_24h: 0,
        payments_24h: 0,
        revenue_24h: 0.0,
        trials_24h: 0,
    });
    Ok(warp::reply::json(&overview))
}

async fn build_overview(state: Arc<AppState>) -> Result<OverviewResponse, reqwest::Error> {
    let now = chrono::Utc::now().timestamp_millis();
    let from_ms = now - 24 * 60 * 60 * 1000;

    let pixel_url = format!(
        "{}/api/metrics?from_ms={}&to_ms={}",
        state.config.pixel.endpoint, from_ms, now
    );
    let mut pixel_req = state.http.get(&pixel_url);
    if let Some(token) = &state.config.pixel.token {
        pixel_req = pixel_req.header("Authorization", format!("Bearer {}", token));
    }
    let visits_24h = match pixel_req.send().await {
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
        "{}/analytics/sales?period=1&granularity=daily",
        state.config.payment.endpoint
    );
    let payment_req = state
        .http
        .get(&payment_url)
        .header(
            "Authorization",
            format!("Bearer {}", state.config.payment.analytics_token),
        );
    let (payments_24h, revenue_24h) = match payment_req.send().await {
        Ok(resp) => {
            let data: serde_json::Value = resp.json().await.unwrap_or_default();
            extract_latest_sales(&data)
        }
        Err(err) => {
            tracing::warn!("Failed to fetch payment sales: {}", err);
            (0, 0.0)
        }
    };

    // Trials from mrkting (24h = period 1 daily)
    let trials_url = format!(
        "{}/analytics/trials?period=1&granularity=daily",
        state.config.mrkting.endpoint
    );
    let mut trials_req = state.http.get(&trials_url);
    if let Some(token) = &state.config.mrkting.token {
        trials_req = trials_req.header("Authorization", format!("Bearer {}", token));
    }
    let trials_24h = match trials_req.send().await {
        Ok(resp) => {
            let data: serde_json::Value = resp.json().await.unwrap_or_default();
            data.get("totals")
                .and_then(|t| t.get("trials"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        }
        Err(err) => {
            tracing::warn!("Failed to fetch trials: {}", err);
            0
        }
    };

    Ok(OverviewResponse {
        visits_24h,
        payments_24h,
        revenue_24h,
        trials_24h,
    })
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

fn extract_latest_sales(data: &serde_json::Value) -> (u64, f64) {
    if let Some(totals) = data.get("totals") {
        let confirmed = totals
            .get("confirmed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let revenue = totals
            .get("revenue")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        return (confirmed, revenue);
    }
    (0, 0.0)
}

async fn sales_proxy_handler(
    query: std::collections::HashMap<String, String>,
    state: Arc<AppState>,
) -> Result<impl warp::Reply, Infallible> {
    let mut url = format!("{}/analytics/sales", state.config.payment.endpoint);
    let mut first = true;
    for (k, v) in query {
        url.push_str(if first { "?" } else { "&" });
        url.push_str(&k);
        url.push('=');
        url.push_str(&urlencoding::encode(&v));
        first = false;
    }

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
            let body: Vec<u8> = r.bytes().await.unwrap_or_default().to_vec();
            Ok(warp::reply::with_status(body, status))
        }
        Err(err) => {
            tracing::error!("Payment sales proxy error: {}", err);
            Ok(warp::reply::with_status(
                format!("{{\"error\":\"{}\"}}", err).into_bytes(),
                warp::http::StatusCode::BAD_GATEWAY,
            ))
        }
    }
}

async fn pixel_proxy_handler(
    query: std::collections::HashMap<String, String>,
    state: Arc<AppState>,
) -> Result<impl warp::Reply, Infallible> {
    let mut url = format!("{}/api/metrics", state.config.pixel.endpoint);
    let mut first = true;
    for (k, v) in query {
        url.push_str(if first { "?" } else { "&" });
        url.push_str(&k);
        url.push('=');
        url.push_str(&urlencoding::encode(&v));
        first = false;
    }

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
