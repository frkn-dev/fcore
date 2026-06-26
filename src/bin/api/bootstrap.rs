use openssl::pkey::PKey;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing_subscriber::Layer;

use fcore::{
    utils::level_from_settings, utils::measure_time, Connection, ConnectionApiOperations,
    ConnectionBaseOperations, MetricStorage, NodeStorageOperations, Publisher, Result,
    Subscription, SubscriptionOperations,
};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{
    filter::{filter_fn, Targets},
    fmt,
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

use super::{
    config::ServiceSettings,
    email::EmailStore,
    postgres::pg::PgContext,
    service::{Cache, Service},
    sync::MemSync,
    tasks::Tasks,
};

impl<N, C, S> Service<N, C, S>
where
    N: NodeStorageOperations + Send + Sync + Clone + 'static + std::default::Default,
    C: ConnectionBaseOperations
        + ConnectionApiOperations
        + Send
        + Sync
        + Clone
        + 'static
        + PartialEq
        + From<Connection>
        + Into<Connection>
        + serde::Serialize
        + 'static,
    S: SubscriptionOperations
        + Send
        + Sync
        + Clone
        + 'static
        + PartialEq
        + Default
        + From<Subscription>
        + 'static,
    Connection: From<C>,
    Vec<(uuid::Uuid, Connection)>: FromIterator<(uuid::Uuid, C)>,
{
    pub async fn bootstrap(settings: ServiceSettings) -> Result<Arc<Service<N, C, S>>> {
        let db = PgContext::init(&settings.pg).await?;

        let mem = Arc::new(RwLock::new(Cache::new()));
        let publisher = Publisher::new(&settings.service.updates_endpoint_zmq).await?;
        let mem_sync = MemSync::new(mem.clone(), db.clone(), publisher);
        let metric_storage = match MetricStorage::load_snapshot(
            &settings.metrics.snapshot_path,
            settings.metrics.max_points,
            settings.metrics.retention_seconds,
        )
        .await
        {
            Ok(storage) => {
                tracing::info!("Metrics snapshot restored");
                storage
            }
            Err(err) => {
                tracing::warn!("Snapshot restore failed: {}", err);

                MetricStorage::new(
                    settings.metrics.max_points,
                    settings.metrics.retention_seconds,
                )
            }
        };

        let email_store = EmailStore::new(settings.smtp.clone());
        email_store.load_trials().await?;

        let agw_private_key = settings
            .service
            .agw_private_key_path
            .as_ref()
            .filter(|p| !p.is_empty())
            .map(|p| {
                let pem = std::fs::read(p)
                    .map_err(|e| fcore::Error::Custom(format!("Failed to read AGW private key: {e}")))?;
                let key = PKey::private_key_from_pem(&pem)
                    .map_err(|e| fcore::Error::Custom(format!("Failed to parse AGW private key: {e}")))?;
                Ok::<_, fcore::Error>(Arc::new(key))
            })
            .transpose()?;

        let service = Service::new(
            mem_sync,
            settings.clone(),
            Arc::new(metric_storage),
            email_store,
            agw_private_key,
        );

        measure_time(service.get_state_from_db(), "Init PostgreSQL DB").await?;

        Ok(Arc::new(service))
    }
}

fn parse_rotation(rotation: &str) -> Rotation {
    match rotation.to_lowercase().as_str() {
        "minutely" => Rotation::MINUTELY,
        "hourly" => Rotation::HOURLY,
        "daily" => Rotation::DAILY,
        "never" => Rotation::NEVER,
        _ => Rotation::DAILY,
    }
}

fn parse_level(level: &str) -> Option<tracing::Level> {
    match level.to_lowercase().as_str() {
        "trace" => Some(tracing::Level::TRACE),
        "debug" => Some(tracing::Level::DEBUG),
        "info" => Some(tracing::Level::INFO),
        "warn" | "warning" => Some(tracing::Level::WARN),
        "error" => Some(tracing::Level::ERROR),
        _ => None,
    }
}

pub fn init_tracing(settings: ServiceSettings) {
    let level = level_from_settings(&settings.service.log_level);

    let stdout_layer = fmt::layer()
        .with_target(true)
        .with_filter(level)
        .with_filter(filter_fn(|metadata| {
            !metadata.target().starts_with("metrics") && !metadata.target().starts_with("sqlx")
        }));

    let registry = tracing_subscriber::registry().with(stdout_layer);

    if settings.metrics.log.enabled {
        let log_directory = settings.metrics.log.directory;
        let log_file = settings.metrics.log.file;
        let rotation = parse_rotation(&settings.metrics.log.rotation);
        let metrics_level =
            parse_level(&settings.metrics.log.level).unwrap_or(tracing::Level::INFO);

        let metrics_file = RollingFileAppender::new(rotation, log_directory, log_file);

        let metrics_layer = fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_writer(metrics_file)
            .with_filter(
                Targets::new()
                    .with_target("metrics", metrics_level)
                    .with_target("metrics.ingest", metrics_level)
                    .with_target("metrics.gc", metrics_level)
                    .with_target("metrics.heartbeat", metrics_level),
            );

        registry.with(metrics_layer).init();
    } else {
        registry.init();
    }
}
