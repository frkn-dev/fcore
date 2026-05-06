use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::Duration;

use fcore::{
    utils::level_from_settings, utils::measure_time, MetricStorage, Publisher, Result, Settings,
    Subscriber, Topic, BANNER, VERSION,
};

use tracing::{debug, error, info};

use crate::{
    config::ServiceSettings,
    http::routes::Http,
    metrics::MetricWorker,
    postgres::pg::PgContext,
    service::{Cache, Service, State},
    sync::MemSync,
    tasks::Tasks,
};

mod config;
mod email;
mod http;
mod metrics;
mod postgres;
mod service;
mod sync;
mod tasks;

#[tokio::main]
async fn main() -> Result<()> {
    println!(">>> API Service {}", VERSION);
    println!("{}", BANNER);

    #[cfg(feature = "debug")]
    console_subscriber::init();

    let config_path = &std::env::args()
        .nth(1)
        .expect("required config path as an argument");
    println!("Config file {:?}", config_path);

    let settings = ServiceSettings::from_file(config_path);

    settings.validate().expect("Wrong settings file");
    println!(">>> Settings: {:?}", settings.clone());

    tracing_subscriber::fmt()
        .with_env_filter(level_from_settings(&settings.service.log_level))
        .init();

    let db = match PgContext::init(&settings.pg).await {
        Ok(db) => db,
        Err(err) => {
            error!("Failed to init DB: {}", err);
            return Err(err);
        }
    };

    let mem: Arc<RwLock<State>> = Arc::new(RwLock::new(Cache::new()));
    let publisher: Publisher = Publisher::new(&settings.service.updates_endpoint_zmq).await?;
    let mem_sync = MemSync::new(mem.clone(), db.clone(), publisher);
    let metric_storage = Arc::new(MetricStorage::new(
        settings.metrics.max_points,
        settings.metrics.retention_seconds,
    ));
    let email_store = email::EmailStore::new(settings.smtp.clone());
    email_store.load_trials().await?;

    let api_service = Arc::new(Service::new(
        mem_sync.clone(),
        settings.clone(),
        metric_storage,
        email_store.clone(),
    ));

    measure_time(api_service.get_state_from_db(), "Init PostgreSQL DB").await?;

    let api_service_clone = api_service.clone();
    tokio::spawn(async move {
        api_service_clone
            .periodic_db_sync(settings.tasks.db_sync_interval_sec)
            .await;
    });

    debug!("Running metrics reciever task");

    let subscriber: Subscriber =
        Subscriber::new_bound(&settings.metrics.reciever, vec![Topic::Metrics])?;

    MetricWorker::start(api_service.metrics.clone(), subscriber).await;

    info!("Metrics system initialized via MetricWorker");

    let metrics_storage = api_service.metrics.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let s = metrics_storage.clone();
            tokio::task::spawn_blocking(move || {
                debug!("Starting MetricStorage GC...");
                s.perform_gc();
                debug!("MetricStorage GC finished.");
            });
        }
    });

    tokio::spawn({
        let api_service = api_service.clone();

        let job_interval = Duration::from_secs(60);
        info!("cleanup_expired_connections task started");

        async move {
            api_service
                .cleanup_expired_connections(job_interval.as_secs())
                .await;
        }
    });

    tokio::spawn({
        let api_service = api_service.clone();
        let job_interval = Duration::from_secs(settings.tasks.subscription_expire_interval);
        info!("cleanup_expired_subscriptions task started");

        async move {
            api_service
                .cleanup_expired_subscriptions(job_interval.as_secs())
                .await;
        }
    });

    tokio::spawn({
        let api_service = api_service.clone();
        let job_interval = Duration::from_secs(settings.tasks.subscription_restore_interval);
        info!("restore_subscriptions task started");

        async move {
            api_service
                .restore_subscriptions(job_interval.as_secs())
                .await;
        }
    });

    let api_service = api_service.clone();
    let service_settings = settings.service.clone();
    let service_handle = tokio::spawn(async move {
        if let Err(e) = api_service.run(service_settings).await {
            error!("API server exited with error: {}", e);
        }
    });

    let res: Result<()> = tokio::select! {
        _ = service_handle => {
            println!("API server finished");
            Ok(())
        }
        _ = tokio::signal::ctrl_c() => {
            println!("Ctrl+C received, shutting down...");
            Ok(())
        }
    };
    res
}
