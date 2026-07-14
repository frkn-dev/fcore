use std::sync::Arc;
use warp::Filter;

use fcore::http::{filters::auth, AuthError};

use super::{
    handlers::{
        get_check_ref_code_handler, get_conversions_handler, get_referral_stats_handler,
        get_referrals_handler, get_subscription_by_ref_code_handler, get_subscription_trial_handler,
        get_trials_handler, get_validate_ref_code_handler, healthcheck_handler, post_account_handler,
        post_create_campaign_handler, post_restock_campaign_handler, post_subscription_extend_handler,
        post_survey_reward_handler, AppState,
    },
    request::{
        AccountRequest, CreateCampaignRequest, RefCodeQuery, RestockRequest,
        SubscriptionExtendRequest, SubscriptionIdQuery, SurveyRewardRequest, TrialsQuery,
    },
};

pub fn routes(
    state: AppState,
) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
    let cors_origins = state.settings.service.cors_origins.clone();
    let auth_filter = auth(Arc::new(state.settings.service.token.clone()));
    let with_state = warp::any().map(move || state.clone());

    let healthcheck = warp::get()
        .and(warp::path("healthcheck"))
        .and(warp::path::end())
        .and_then(healthcheck_handler);

    let account = warp::post()
        .and(warp::path("account"))
        .and(warp::path::end())
        .and(with_state.clone())
        .and(warp::body::json::<AccountRequest>())
        .and(warp::header::optional::<String>("x-trace-id"))
        .and_then(post_account_handler);

    let referrals = warp::get()
        .and(warp::path("referrals"))
        .and(warp::path::end())
        .and(with_state.clone())
        .and(warp::query::<RefCodeQuery>())
        .and_then(get_referral_stats_handler);

    let validate = warp::get()
        .and(warp::path("validate"))
        .and(warp::path("ref_code"))
        .and(warp::path::end())
        .and(auth_filter.clone())
        .and(with_state.clone())
        .and(warp::query::<RefCodeQuery>())
        .and_then(get_validate_ref_code_handler);

    let check = warp::get()
        .and(warp::path("check"))
        .and(warp::path("ref_code"))
        .and(warp::path::end())
        .and(with_state.clone())
        .and(warp::query::<RefCodeQuery>())
        .and_then(get_check_ref_code_handler);

    let mut cors_builder = warp::cors()
        .allow_methods(vec!["GET", "POST", "OPTIONS"])
        .allow_headers(vec!["Content-Type", "Authorization", "X-Trace-Id"])
        .allow_credentials(true)
        .max_age(86400);

    for origin in &cors_origins {
        cors_builder = cors_builder.allow_origin(origin.as_str());
    }

    let cors = cors_builder.build();

    let subscription_by_ref_code = warp::get()
        .and(warp::path("subscription"))
        .and(warp::path("by_ref_code"))
        .and(warp::path::end())
        .and(auth_filter.clone())
        .and(with_state.clone())
        .and(warp::query::<RefCodeQuery>())
        .and_then(get_subscription_by_ref_code_handler);

    let surveys_reward = warp::post()
        .and(warp::path("surveys"))
        .and(warp::path("reward"))
        .and(warp::path::end())
        .and(with_state.clone())
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::body::json::<SurveyRewardRequest>())
        .and_then(post_survey_reward_handler);

    let surveys_create_campaign = warp::post()
        .and(warp::path("surveys"))
        .and(warp::path("campaigns"))
        .and(warp::path::end())
        .and(auth_filter.clone())
        .and(with_state.clone())
        .and(warp::body::json::<CreateCampaignRequest>())
        .and_then(post_create_campaign_handler);

    let surveys_restock_campaign = warp::post()
        .and(warp::path("surveys"))
        .and(warp::path("campaigns"))
        .and(warp::path::param::<String>())
        .and(warp::path("restock"))
        .and(warp::path::end())
        .and(auth_filter.clone())
        .and(with_state.clone())
        .and(warp::body::json::<RestockRequest>())
        .and_then(|name: String, state: AppState, req: RestockRequest| {
            post_restock_campaign_handler(state, name, req)
        });

    let subscription_extend = warp::post()
        .and(warp::path("subscription"))
        .and(warp::path("extend"))
        .and(warp::path::end())
        .and(auth_filter.clone())
        .and(with_state.clone())
        .and(warp::body::json::<SubscriptionExtendRequest>())
        .and_then(post_subscription_extend_handler);

    let subscription_trial = warp::get()
        .and(warp::path("subscription"))
        .and(warp::path("trial"))
        .and(warp::path::end())
        .and(auth_filter.clone())
        .and(with_state.clone())
        .and(warp::query::<SubscriptionIdQuery>())
        .and_then(get_subscription_trial_handler);

    let referrals_analytics = warp::get()
        .and(warp::path("analytics"))
        .and(warp::path("referrals"))
        .and(warp::path::end())
        .and(auth_filter.clone())
        .and(with_state.clone())
        .and(warp::query::<TrialsQuery>())
        .and_then(get_referrals_handler);

    let conversions = warp::get()
        .and(warp::path("analytics"))
        .and(warp::path("conversions"))
        .and(warp::path::end())
        .and(auth_filter.clone())
        .and(with_state.clone())
        .and(warp::query::<TrialsQuery>())
        .and_then(get_conversions_handler);

    let trials = warp::get()
        .and(warp::path("analytics"))
        .and(warp::path("trials"))
        .and(warp::path::end())
        .and(auth_filter)
        .and(with_state.clone())
        .and(warp::query::<TrialsQuery>())
        .and_then(get_trials_handler);

    healthcheck
        .or(account)
        .or(surveys_reward)
        .or(surveys_create_campaign)
        .or(surveys_restock_campaign)
        .or(referrals)
        .or(validate)
        .or(check)
        .or(subscription_by_ref_code)
        .or(subscription_extend)
        .or(subscription_trial)
        .or(conversions)
        .or(referrals_analytics)
        .or(trials)
        .recover(handle_rejection)
        .with(cors)
        .with(warp::log("mrkting"))
}

async fn handle_rejection(err: warp::Rejection) -> Result<impl warp::Reply, std::convert::Infallible> {
    if err.find::<AuthError>().is_some() {
        tracing::debug!("AuthError rejection");
        Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"status": 401, "message": "Unauthorized"}),
            ),
            warp::http::StatusCode::UNAUTHORIZED,
        ))
    } else if err.is_not_found() {
        Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"status": 404, "message": "Not found"}),
            ),
            warp::http::StatusCode::NOT_FOUND,
        ))
    } else {
        tracing::warn!("Unhandled rejection: {:?}", err);
        Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"status": 500, "message": "Internal server error"}),
            ),
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        ))
    }
}
