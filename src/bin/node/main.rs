mod config;
mod http;
mod metrics;
mod metrics_http;
mod node;
mod snapshot;
#[cfg(feature = "xray")]
mod stats;
mod tasks;

use config::ServiceSettings;
use fcore::{utils::level_from_settings, Settings, BANNER, VERSION};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    fcore::metrics::mark_process_start();
    println!(">>> Node {}", VERSION);
    println!("{}", BANNER);

    #[cfg(feature = "debug")]
    console_subscriber::init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let reset_snapshot = args.iter().any(|a| a == "--reset-snapshot");
    let config_path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .expect("required config path as an argument")
        .clone();
    println!("Config file {}", config_path);

    let settings = ServiceSettings::from_file(&config_path);

    settings.validate().expect("Wrong settings file");
    // Never print the whole settings struct: it carries the api token and the
    // node's private keys, and journalctl used to keep them forever.
    println!(
        ">>> Settings: env={:?} hostname={:?} pool={:?} snapshot={}",
        settings.node.env,
        settings.node.hostname,
        settings.node.pool,
        settings.service.snapshot_path,
    );

    if reset_snapshot {
        let path = std::path::Path::new(&settings.service.snapshot_path);
        if !path.exists() {
            println!(">>> --reset-snapshot: no snapshot at {}", path.display());
            return Ok(());
        }
        let backup = format!(
            "{}.bak-{}",
            path.display(),
            chrono::Utc::now().timestamp()
        );
        std::fs::rename(path, &backup)?;
        println!(">>> --reset-snapshot: moved {} -> {}", path.display(), backup);
        println!(">>> next start begins from a clean state");
        return Ok(());
    }

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

    runtime.block_on(node::run(settings))?;

    Ok(())
}
