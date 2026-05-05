mod config;
mod handlers;
mod http;
mod metrics;
mod request;
mod response;
mod service;
mod tasks;

use config::ServiceSettings;
use fcore::{utils::level_from_settings, Settings, BANNER, VERSION};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(">>> Auth Service {}", VERSION);

    println!("{}", BANNER);

    #[cfg(feature = "debug")]
    console_subscriber::init();

    let config_path = &std::env::args()
        .nth(1)
        .expect("required config path as an argument");
    println!("Config file {}", config_path);

    let settings = ServiceSettings::from_file(config_path);

    settings.validate().expect("Wrong settings file");
    println!(">>> Settings: {:?}", settings.clone());

    tracing_subscriber::fmt()
        .with_env_filter(level_from_settings(&settings.service.log_level))
        .init();

    let num_cpus = std::thread::available_parallelism()?.get();

    let worker_threads = if num_cpus <= 1 { 1 } else { num_cpus * 2 };
    tracing::info!(
        "🧠 CPU cores: {}, configured worker threads: {}",
        num_cpus,
        worker_threads
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(service::run(settings))?;

    Ok(())
}
