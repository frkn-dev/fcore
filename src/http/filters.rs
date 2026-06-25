use std::sync::Arc;
use warp::Filter;
use warp::Rejection;

use crate::http::AuthError;

pub fn auth(token: Arc<String>) -> impl Filter<Extract = (), Error = Rejection> + Clone {
    warp::header::<String>("authorization")
        .and_then(move |auth_header: String| {
            let token = token.clone();
            async move {
                if auth_header
                    .strip_prefix("Bearer ")
                    .is_some_and(|t| t == token.as_str())
                {
                    Ok(())
                } else {
                    Err(warp::reject::custom(AuthError("Unauthorized".to_string())))
                }
            }
        })
        .untuple_one()
}

pub fn with_http_client(
    client: reqwest::Client,
) -> impl Filter<Extract = (reqwest::Client,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || client.clone())
}

pub fn with_param_string(
    param: String,
) -> impl Filter<Extract = (String,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || param.clone())
}

pub fn with_u16(v: u16) -> impl Filter<Extract = (u16,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || v)
}

pub fn with_i64(v: i64) -> impl Filter<Extract = (i64,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || v)
}

pub fn with_param_bool(
    v: bool,
) -> impl Filter<Extract = (bool,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || v)
}
