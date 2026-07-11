use warp::Filter;

use super::{
    handlers::{
        get_referral_stats_handler, get_validate_ref_code_handler, healthcheck_handler,
        post_account_handler, AppState,
    },
    request::{AccountRequest, RefCodeQuery},
};

pub fn routes(
    state: AppState,
) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
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
        .and(with_state.clone())
        .and(warp::query::<RefCodeQuery>())
        .and_then(get_validate_ref_code_handler);

    healthcheck.or(account).or(referrals).or(validate)
}
