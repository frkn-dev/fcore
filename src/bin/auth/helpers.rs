use serde::Deserialize;

use fcore::{
    http::response::{Instance, InstanceWithId, ResponseMessage, SubscriptionResponse},
    Code, Env, Error, Key, Result, Subscription, Tag,
};

use super::http::HttpClient;

fn auth_headers(req: reqwest::RequestBuilder, api_token: &str) -> reqwest::RequestBuilder {
    req.header("Authorization", format!("Bearer {}", api_token))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
}
