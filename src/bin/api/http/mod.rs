use warp::reject;
use warp::{http::StatusCode, Rejection, Reply};

use fcore::http::{AuthError, MethodError};

mod filters;
pub(crate) mod handlers;
pub(crate) mod param;
pub(crate) mod request;
pub(crate) mod routes;

#[derive(Debug)]
struct JsonError(String);
impl reject::Reject for JsonError {}

pub async fn rejection(reject: Rejection) -> Result<impl Reply, Rejection> {
    tracing::debug!("[REJECTION] Request rejected: {:?}", reject);
    if reject.find::<MethodError>().is_some() {
        tracing::debug!("[REJECTION] Reason: Method Not Allowed");
        let error_response = warp::reply::json(&serde_json::json!({
            "error": "Method Not Allowed"
        }));
        Ok(warp::reply::with_status(
            error_response,
            StatusCode::METHOD_NOT_ALLOWED,
        ))
    } else if reject.find::<AuthError>().is_some() {
        tracing::debug!("[REJECTION] Reason UNAUTHORIZED");
        let error_response = warp::reply::json(&serde_json::json!({
            "error": "UNAUTHORIZED"
        }));
        Ok(warp::reply::with_status(
            error_response,
            StatusCode::UNAUTHORIZED,
        ))
    } else if let Some(err) = reject.find::<JsonError>() {
        tracing::debug!("[REJECTION] Reason BAD_REQUEST");

        let error_response = warp::reply::json(&serde_json::json!({
            "error": err.0
        }));
        Ok(warp::reply::with_status(
            error_response,
            StatusCode::BAD_REQUEST,
        ))
    } else if reject.is_not_found() {
        tracing::debug!("[REJECTION] Reason Not_Found");
        let error_response = warp::reply::json(&serde_json::json!({
            "error": "Not Found"
        }));
        Ok(warp::reply::with_status(
            error_response,
            StatusCode::NOT_FOUND,
        ))
    } else {
        Err(reject)
    }
}
