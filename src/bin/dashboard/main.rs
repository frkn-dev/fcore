mod config;
mod partner;
mod payment_client;
mod postgres;
mod server;

#[tokio::main]
async fn main() {
    let config_path = std::env::args()
        .nth(1)
        .expect("Usage: dashboard <config.toml>");

    let config = config::Config::from_file(&config_path);

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let pg = match postgres::PgContext::init(&config.pg).await {
        Ok(pg) => pg,
        Err(e) => {
            eprintln!("Failed to initialize postgres: {}", e);
            std::process::exit(1);
        }
    };

    server::start_server(config, pg).await;
}
