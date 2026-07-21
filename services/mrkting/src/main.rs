use std::sync::Arc;

use base64::Engine;
use crate::common::{Result, Settings, VERSION};

use crate::{
    api_client::ApiClient,
    config::ServiceSettings,
    crypto::EmailCipher,
    email::Mailer,
    http::{handlers::AppState, routes::routes},
    postgres::PgContext,
};

mod api_client;
mod common;
mod config;
mod crypto;
mod email;
mod http;
mod postgres;

#[tokio::main]
async fn main() -> Result<()> {
    println!(">>> Marketing Service {}", VERSION);

    let config_path = std::env::args()
        .nth(1)
        .expect("required config path as an argument");

    let settings = ServiceSettings::from_file(&config_path);
    settings.validate().expect("Wrong settings file");

    init_tracing(&settings.service.log_level);

    let pg = PgContext::init(&settings.pg).await?;
    let email_key = base64::engine::general_purpose::STANDARD
        .decode(&settings.email_encryption.key)
        .expect("email_encryption.key must be valid base64");
    let cipher = Arc::new(EmailCipher::new(&email_key));
    let mailer = Arc::new(Mailer::new(&settings.smtp));
    let api_client = Arc::new(ApiClient::new(&settings.api));

    let state = AppState {
        settings: settings.clone(),
        pg,
        cipher,
        mailer,
        api_client,
        rate_limiter: Arc::new(crate::http::handlers::RateLimiter::new(
            30,
            std::time::Duration::from_secs(60),
        )),
    };

    let addr = (settings.service.listen, settings.service.port);
    tracing::info!("Marketing service listening on {:?}", addr);

    warp::serve(routes(state)).run(addr).await;

    Ok(())
}

fn init_tracing(level: &str) {
    let filter = crate::common::level_from_settings(level);
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
