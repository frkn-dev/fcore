use crate::config::Config;
use crate::partner;
use crate::postgres::{PartnerRow, PgContext};
use chrono::Datelike;
use serde::Serialize;
use std::convert::Infallible;
use std::sync::Arc;
use uuid::Uuid;
use warp::Filter;

const ADMIN_HTML: &str = include_str!("admin.html");
const PARTNER_HTML: &str = include_str!("partner.html");

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub http: reqwest::Client,
    pub pg: PgContext,
}

#[derive(Serialize)]
pub struct OverviewResponse {
    pub visits: u64,
    pub payments: u64,
    pub revenue: f64,
    pub trials: u64,
    pub conversions: u64,
    pub referrals: u64,
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
pub enum OverviewPeriod {
    Today,
    Yesterday,
    Last24h,
    Last7d,
    Last30d,
    Last90d,
    ThisMonth,
}

impl OverviewPeriod {
    pub fn from_str(s: &str) -> Option<Self> {
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
    pub fn bounds(&self) -> (i64, i64, i64) {
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

pub async fn start_server(config: Config, pg: PgContext) {
    let state = Arc::new(AppState {
        config,
        http: reqwest::Client::new(),
        pg,
    });

    let index = warp::path::end()
        .and(warp::get())
        .map(|| warp::reply::html(ADMIN_HTML));

    let partner_page = warp::path("partner")
        .and(warp::path::end())
        .and(warp::get())
        .map(|| warp::reply::html(PARTNER_HTML));

    let api_auth = dashboard_api_auth_filter(state.clone());

    let api_overview = warp::path("api")
        .and(warp::path("overview"))
        .and(warp::get())
        .and(warp::path::end())
        .and(api_auth.clone())
        .and(warp::query::<OverviewQuery>())
        .and(with_state(state.clone()))
        .and_then(overview_handler);

    let api_sales = warp::path("api")
        .and(warp::path("sales"))
        .and(warp::get())
        .and(warp::path::end())
        .and(api_auth.clone())
        .and(warp::query::<PeriodQuery>())
        .and(with_state(state.clone()))
        .and_then(sales_proxy_handler);

    let api_pixel = warp::path("api")
        .and(warp::path("pixel"))
        .and(warp::get())
        .and(warp::path::end())
        .and(api_auth.clone())
        .and(warp::query::<PeriodQuery>())
        .and(with_state(state.clone()))
        .and_then(pixel_proxy_handler);

    let health = warp::path("health")
        .and(warp::get())
        .and(warp::path::end())
        .map(|| "ok");

    let partner_routes = partner_routes(state.clone());

    let routes = index
        .or(partner_page)
        .or(partner_routes)
        .or(api_overview)
        .or(api_sales)
        .or(api_pixel)
        .or(health);

    let cors = warp::cors()
        .allow_methods(vec!["GET", "POST", "DELETE", "OPTIONS"])
        .allow_headers(vec!["Content-Type", "Authorization"])
        .allow_any_origin();

    let routes = routes.with(cors).recover(handle_rejection);

    let addr: std::net::SocketAddr = format!(
        "{}:{}",
        state.config.dashboard.listen, state.config.dashboard.port
    )
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

fn dashboard_api_auth_filter(
    state: Arc<AppState>,
) -> impl Filter<Extract = (), Error = warp::Rejection> + Clone {
    warp::header::optional::<String>("authorization")
        .and(with_state(state))
        .and_then(|auth: Option<String>, state: Arc<AppState>| async move {
            match &state.config.dashboard.api_token {
                Some(expected) => {
                    let provided = auth
                        .as_deref()
                        .and_then(|h| h.strip_prefix("Bearer "))
                        .unwrap_or(auth.as_deref().unwrap_or_default());
                    if provided == expected {
                        Ok(())
                    } else {
                        Err(warp::reject::custom(DashboardAuthRejection))
                    }
                }
                None => Ok(()),
            }
        })
        .untuple_one()
}

#[derive(Debug)]
struct DashboardAuthRejection;

impl warp::reject::Reject for DashboardAuthRejection {}

fn dashboard_admin_auth_filter(
    state: Arc<AppState>,
) -> impl Filter<Extract = (), Error = warp::Rejection> + Clone {
    warp::header::optional::<String>("authorization")
        .and(with_state(state))
        .and_then(|auth: Option<String>, state: Arc<AppState>| async move {
            match &state.config.dashboard.admin_token {
                Some(expected) => {
                    let provided = auth
                        .as_deref()
                        .and_then(|h| h.strip_prefix("Bearer "))
                        .unwrap_or(auth.as_deref().unwrap_or_default());
                    if provided == expected {
                        Ok(())
                    } else {
                        Err(warp::reject::custom(DashboardAuthRejection))
                    }
                }
                None => Err(warp::reject::custom(DashboardAuthRejection)),
            }
        })
        .untuple_one()
}

async fn handle_rejection(err: warp::Rejection) -> Result<impl warp::Reply, std::convert::Infallible> {
    if err.find::<DashboardAuthRejection>().is_some() {
        Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"success": false, "message": "Unauthorized"}),
            ),
            warp::http::StatusCode::UNAUTHORIZED,
        ))
    } else if err.is_not_found() {
        Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"success": false, "message": "Not found"}),
            ),
            warp::http::StatusCode::NOT_FOUND,
        ))
    } else if let Some(e) = err.find::<warp::reject::MethodNotAllowed>() {
        tracing::warn!("Method not allowed: {:?}", e);
        Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"success": false, "message": "Method not allowed"}),
            ),
            warp::http::StatusCode::METHOD_NOT_ALLOWED,
        ))
    } else {
        tracing::error!("Unhandled rejection: {:?}", err);
        Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"success": false, "message": "Internal server error"}),
            ),
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        ))
    }
}

fn partner_auth_filter(
    state: Arc<AppState>,
) -> impl Filter<Extract = (PartnerRow,), Error = warp::Rejection> + Clone {
    warp::header::<String>("authorization")
        .and(with_state(state))
        .and_then(|auth: String, state: Arc<AppState>| async move {
            let token = auth.strip_prefix("Bearer ").unwrap_or(&auth).to_string();
            tracing::debug!("Partner auth attempt with token: {}", token);
            match state.pg.sessions().find_by_token(&token).await {
                Ok(Some(session)) => match state.pg.partners().find_by_id(session.partner_id).await {
                    Ok(Some(partner)) => {
                        if partner.active {
                            tracing::debug!("Partner auth success for partner_id: {}", partner.id);
                            Ok(partner)
                        } else {
                            tracing::warn!("Partner auth failed: account disabled for token: {}", token);
                            Err(warp::reject::not_found())
                        }
                    }
                    Ok(None) => {
                        tracing::warn!("Partner auth failed: partner not found for token: {}", token);
                        Err(warp::reject::not_found())
                    }
                    Err(e) => {
                        tracing::error!("Partner auth DB error finding partner: {}", e);
                        Err(warp::reject::not_found())
                    }
                },
                Ok(None) => {
                    tracing::warn!("Partner auth failed: session not found for token: {}", token);
                    Err(warp::reject::not_found())
                }
                Err(e) => {
                    tracing::error!("Partner auth DB error finding session: {}", e);
                    Err(warp::reject::not_found())
                }
            }
        })
}

fn partner_routes(
    state: Arc<AppState>,
) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
    let login = warp::path("api")
        .and(warp::path("partner"))
        .and(warp::path("login"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_state(state.clone()))
        .and(warp::body::json::<partner::PartnerLoginRequest>())
        .and_then(|_state: Arc<AppState>, req: partner::PartnerLoginRequest| {
            partner::login_handler(partner_state_for(_state), req)
        });

    let create_partner = warp::path("api")
        .and(warp::path("partner"))
        .and(warp::path("create"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_state(state.clone()))
        .and(warp::body::json::<partner::CreatePartnerRequest>())
        .and_then(|_state: Arc<AppState>, req: partner::CreatePartnerRequest| {
            partner::create_partner_handler(partner_state_for(_state), req)
        });

    let auth_filter = partner_auth_filter(state.clone());

    let me = warp::path("api")
        .and(warp::path("partner"))
        .and(warp::path("me"))
        .and(warp::path::end())
        .and(warp::get())
        .and(auth_filter.clone())
        .and(with_state(state.clone()))
        .and_then(|partner: PartnerRow, state: Arc<AppState>| {
            partner::me_handler(partner_state_for(state), partner)
        });

    let create_promocode = warp::path("api")
        .and(warp::path("partner"))
        .and(warp::path("promocodes"))
        .and(warp::path::end())
        .and(warp::post())
        .and(auth_filter.clone())
        .and(with_state(state.clone()))
        .and(warp::body::json::<partner::CreatePartnerPromocodeRequest>())
        .and_then(
            |partner: PartnerRow,
             state: Arc<AppState>,
             req: partner::CreatePartnerPromocodeRequest| {
                partner::create_promocode_handler(partner_state_for(state), partner, req)
            },
        );

    let list_promocodes = warp::path("api")
        .and(warp::path("partner"))
        .and(warp::path("promocodes"))
        .and(warp::path::end())
        .and(warp::get())
        .and(auth_filter.clone())
        .and(with_state(state.clone()))
        .and_then(|partner: PartnerRow, state: Arc<AppState>| {
            partner::list_promocodes_handler(partner_state_for(state), partner)
        });

    let delete_promocode = warp::path("api")
        .and(warp::path("partner"))
        .and(warp::path("promocodes"))
        .and(warp::path::param::<Uuid>())
        .and(warp::path::end())
        .and(warp::delete())
        .and(auth_filter.clone())
        .and(with_state(state.clone()))
        .and_then(|id: Uuid, partner: PartnerRow, state: Arc<AppState>| {
            partner::delete_promocode_handler(partner_state_for(state), partner, id)
        });

    let admin_auth = dashboard_admin_auth_filter(state.clone());

    let attach_promocode = warp::path("api")
        .and(warp::path("partner"))
        .and(warp::path("admin"))
        .and(warp::path("attach-promocode"))
        .and(warp::path::end())
        .and(warp::post())
        .and(admin_auth.clone())
        .and(with_state(state.clone()))
        .and(warp::body::json::<partner::AttachPartnerPromocodeRequest>())
        .and_then(|state: Arc<AppState>, req: partner::AttachPartnerPromocodeRequest| {
            partner::attach_promocode_handler(partner_state_for(state), req)
        });

    let partner_overview = warp::path("api")
        .and(warp::path("partner"))
        .and(warp::path("analytics"))
        .and(warp::path("overview"))
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<PeriodQuery>())
        .and(auth_filter.clone())
        .and(with_state(state.clone()))
        .and_then(
            |query: PeriodQuery, partner: PartnerRow, state: Arc<AppState>| {
                partner_overview_handler(state, partner, query)
            },
        );

    let partner_sales = warp::path("api")
        .and(warp::path("partner"))
        .and(warp::path("analytics"))
        .and(warp::path("sales"))
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<PeriodQuery>())
        .and(auth_filter.clone())
        .and(with_state(state.clone()))
        .and_then(
            |query: PeriodQuery, partner: PartnerRow, state: Arc<AppState>| {
                partner_sales_handler(state, partner, query)
            },
        );

    let partner_visits = warp::path("api")
        .and(warp::path("partner"))
        .and(warp::path("analytics"))
        .and(warp::path("visits"))
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<PeriodQuery>())
        .and(auth_filter.clone())
        .and(with_state(state.clone()))
        .and_then(
            |query: PeriodQuery, partner: PartnerRow, state: Arc<AppState>| {
                partner_visits_handler(state, partner, query)
            },
        );

    let partner_promocodes_stats = warp::path("api")
        .and(warp::path("partner"))
        .and(warp::path("analytics"))
        .and(warp::path("promocodes"))
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<PeriodQuery>())
        .and(auth_filter.clone())
        .and(with_state(state.clone()))
        .and_then(
            |query: PeriodQuery, partner: PartnerRow, state: Arc<AppState>| {
                partner_promocodes_stats_handler(state, partner, query)
            },
        );

    login
        .or(create_partner)
        .or(me)
        .or(create_promocode)
        .or(list_promocodes)
        .or(delete_promocode)
        .or(attach_promocode)
        .or(partner_overview)
        .or(partner_sales)
        .or(partner_visits)
        .or(partner_promocodes_stats)
}

fn partner_state_for(state: Arc<AppState>) -> partner::PartnerState {
    partner::PartnerState {
        pg: state.pg.clone(),
        config: state.config.clone(),
        http: state.http.clone(),
    }
}

async fn partner_overview_handler(
    state: Arc<AppState>,
    partner: PartnerRow,
    query: PeriodQuery,
) -> Result<impl warp::Reply, Infallible> {
    let period = OverviewPeriod::from_str(&query.period).unwrap_or(OverviewPeriod::Last24h);
    let (from_ms, to_ms, api_period) = period.bounds();

    let codes = match state.pg.promocodes().list(partner.id).await {
        Ok(list) => list.into_iter().map(|p| p.code).collect::<Vec<_>>(),
        Err(e) => {
            tracing::error!("Failed to list partner promocodes: {}", e);
            Vec::new()
        }
    };

    let visits = match fetch_partner_visits(state.clone(), &codes, from_ms, to_ms).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to fetch partner visits: {}", e);
            0
        }
    };

    let (payments, revenue) = match fetch_partner_sales(state.clone(), &codes, api_period, from_ms, to_ms).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to fetch partner sales: {}", e);
            (0, 0.0)
        }
    };

    Ok(warp::reply::json(&serde_json::json!({
        "visits": visits,
        "payments": payments,
        "revenue": revenue,
    }),
    ))
}

async fn partner_sales_handler(
    state: Arc<AppState>,
    partner: PartnerRow,
    query: PeriodQuery,
) -> Result<impl warp::Reply, Infallible> {
    let period = OverviewPeriod::from_str(&query.period).unwrap_or(OverviewPeriod::Last24h);
    let (from_ms, to_ms, api_period) = period.bounds();
    let codes = state.pg.promocodes().list(partner.id).await.unwrap_or_default()
        .into_iter().map(|p| p.code).collect::<Vec<_>>();

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
            filter_sales_by_promocodes(&mut body, &codes, from_ms, to_ms);
            Ok(warp::reply::with_status(warp::reply::json(&body), status))
        }
        Err(err) => {
            tracing::error!("Partner sales proxy error: {}", err);
            Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": err.to_string()})),
                warp::http::StatusCode::BAD_GATEWAY,
            ))
        }
    }
}

async fn partner_visits_handler(
    state: Arc<AppState>,
    partner: PartnerRow,
    query: PeriodQuery,
) -> Result<impl warp::Reply, Infallible> {
    let period = OverviewPeriod::from_str(&query.period).unwrap_or(OverviewPeriod::Last24h);
    let (from_ms, to_ms, _) = period.bounds();
    let codes = state.pg.promocodes().list(partner.id).await.unwrap_or_default()
        .into_iter().map(|p| p.code).collect::<Vec<_>>();

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
            let mut body: serde_json::Value = r.json().await.unwrap_or_default();
            filter_visits_by_promocodes(&mut body, &codes);
            Ok(warp::reply::with_status(warp::reply::json(&body), status))
        }
        Err(err) => {
            tracing::error!("Partner visits proxy error: {}", err);
            Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": err.to_string()})),
                warp::http::StatusCode::BAD_GATEWAY,
            ))
        }
    }
}

async fn partner_promocodes_stats_handler(
    state: Arc<AppState>,
    partner: PartnerRow,
    query: PeriodQuery,
) -> Result<impl warp::Reply, Infallible> {
    let period = OverviewPeriod::from_str(&query.period).unwrap_or(OverviewPeriod::Last24h);
    let (from_ms, to_ms, api_period) = period.bounds();
    let codes = state.pg.promocodes().list(partner.id).await.unwrap_or_default()
        .into_iter().map(|p| p.code).collect::<Vec<_>>();

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

    let mut result = Vec::new();
    let mut totals: std::collections::HashMap<String, (u64, f64)> = std::collections::HashMap::new();

    if let Ok(r) = resp {
        if let Ok(body) = r.json::<serde_json::Value>().await {
            if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
                for bucket in data {
                    if let Some(bucket_str) = bucket.get("bucket").and_then(|b| b.as_str()) {
                        if let Ok(bucket_ms) = parse_bucket_ms(bucket_str) {
                            if bucket_ms >= from_ms && bucket_ms <= to_ms {
                                if let Some(rows) = bucket.get("byPromocode").and_then(|b| b.as_array()) {
                                    for row in rows {
                                        if let Some(code) = row.get("promocode").and_then(|p| p.as_str()) {
                                            if codes.iter().any(|c| c.eq_ignore_ascii_case(code)) {
                                                let count = row.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                                                let revenue = row.get("revenue").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                                let entry = totals.entry(code.to_uppercase()).or_insert((0, 0.0));
                                                entry.0 += count;
                                                entry.1 += revenue;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    for (code, (uses, revenue)) in totals {
        result.push(serde_json::json!({
            "code": code,
            "uses": uses,
            "revenue": revenue,
        }));
    }
    result.sort_by(|a, b| b.get("revenue").and_then(|v| v.as_f64()).unwrap_or(0.0).partial_cmp(&a.get("revenue").and_then(|v| v.as_f64()).unwrap_or(0.0)
    ).unwrap_or(std::cmp::Ordering::Equal));

    Ok(warp::reply::json(&serde_json::json!({
        "success": true,
        "promocodes": result,
    }),
    ))
}

fn filter_sales_by_promocodes(
    body: &mut serde_json::Value,
    codes: &[String],
    from_ms: i64,
    to_ms: i64,
) {
    if let Some(data) = body.get_mut("data").and_then(|d| d.as_array_mut()) {
        for bucket in data.iter_mut() {
            if let Some(bucket_str) = bucket.get("bucket").and_then(|b| b.as_str()) {
                if let Ok(bucket_ms) = parse_bucket_ms(bucket_str) {
                    if bucket_ms >= from_ms && bucket_ms <= to_ms {
                        if let Some(rows) = bucket.get_mut("byPromocode").and_then(|b| b.as_array_mut()) {
                            rows.retain(|row| {
                                row.get("promocode")
                                    .and_then(|p| p.as_str())
                                    .map(|code| codes.iter().any(|c| c.eq_ignore_ascii_case(code)))
                                    .unwrap_or(false)
                            });
                            let (count, revenue) = rows.iter().fold((0u64, 0.0), |(c, r), row| {
                                let cc = row.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                                let rr = row.get("revenue").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                (c + cc, r + rr)
                            });
                            if let Some(obj) = bucket.as_object_mut() {
                                obj.insert("confirmed".to_string(), serde_json::json!(count));
                                obj.insert("revenue".to_string(), serde_json::json!(revenue));
                            }
                        }
                    }
                }
            }
        }
    }
}

fn filter_visits_by_promocodes(body: &mut serde_json::Value, codes: &[String]) {
    if let Some(series) = body.get_mut("series").and_then(|s| s.as_array_mut()) {
        series.retain(|s| {
            if s.get("metric").and_then(|m| m.as_str()) != Some("web.visits.utm_campaign") {
                return true;
            }
            s.get("tags")
                .and_then(|t| t.get("utm_campaign"))
                .and_then(|v| v.as_str())
                .map(|code| codes.iter().any(|c| c.eq_ignore_ascii_case(code)))
                .unwrap_or(false)
        });
    }
}

async fn fetch_partner_visits(
    state: Arc<AppState>,
    codes: &[String],
    from_ms: i64,
    to_ms: i64,
) -> Result<u64, reqwest::Error> {
    let url = format!(
        "{}/api/metrics?from_ms={}&to_ms={}",
        state.config.pixel.endpoint, from_ms, to_ms
    );
    let mut req = state.http.get(&url);
    if let Some(token) = &state.config.pixel.token {
        req = req.header("Authorization", format!("Bearer {}", token));
    }
    let resp = req.send().await?;
    let mut data: serde_json::Value = resp.json().await.unwrap_or_default();
    filter_visits_by_promocodes(&mut data, codes);
    Ok(count_visits(&data))
}

async fn fetch_partner_sales(
    state: Arc<AppState>,
    codes: &[String],
    api_period: i64,
    from_ms: i64,
    to_ms: i64,
) -> Result<(u64, f64), reqwest::Error> {
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
        .await?;
    let mut data: serde_json::Value = resp.json().await.unwrap_or_default();
    filter_sales_by_promocodes(&mut data, codes, from_ms, to_ms);
    Ok(extract_sales_in_range(&data, from_ms, to_ms))
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
        referrals: 0,
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

    let referrals_url = format!(
        "{}/analytics/referrals?period={}&granularity=daily",
        state.config.mrkting.endpoint, api_period
    );
    let mut referrals_req = state.http.get(&referrals_url);
    if let Some(token) = &state.config.mrkting.token {
        referrals_req = referrals_req.header("Authorization", format!("Bearer {}", token));
    }
    let referrals = match referrals_req.send().await {
        Ok(resp) => {
            let data: serde_json::Value = resp.json().await.unwrap_or_default();
            extract_referrals_in_range(&data, from_ms, to_ms)
        }
        Err(err) => {
            tracing::warn!("Failed to fetch referrals: {}", err);
            0
        }
    };

    Ok(OverviewResponse {
        visits,
        payments,
        revenue,
        trials,
        conversions,
        referrals,
    })
}

fn extract_referrals_in_range(data: &serde_json::Value, from_ms: i64, to_ms: i64) -> u64 {
    let mut total = 0u64;
    if let Some(data_arr) = data.get("data").and_then(|d| d.as_array()) {
        for bucket in data_arr {
            if let Some(bucket_str) = bucket.get("bucket").and_then(|b| b.as_str()) {
                if let Ok(bucket_ms) = parse_bucket_ms(bucket_str) {
                    if bucket_ms >= from_ms && bucket_ms <= to_ms {
                        total += bucket
                            .get("referrals")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                    }
                }
            }
        }
    }
    total
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
