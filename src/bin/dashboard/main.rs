mod config;
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

    server::start_server(config).await;
}
