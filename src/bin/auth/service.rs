use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tokio::{
    signal,
    sync::{broadcast, RwLock},
};
use warp::Filter;

use fcore::{
    BaseConnection as Connection, ConnectionBaseOperations, Connections, MetricBuffer, Node,
    NodeConfig, Publisher, Result, SnapshotManager, Subscriber, Tag, Topic,
};

use super::config::ServiceSettings;

use super::handlers::auth_handler;
use super::http::ApiRequests;
use super::request;
use super::tasks::Tasks;

pub struct Service<C>
where
    C: ConnectionBaseOperations + Send + Sync + Clone + 'static,
{
    pub memory: Arc<RwLock<Connections<C>>>,
    pub metrics: Arc<MetricBuffer>,
    pub node: Node,
    pub subscriber: Subscriber,
    pub listen: Ipv4Addr,
    pub port: u16,
    pub origin: String,
}

impl<C> Service<C>
where
    C: ConnectionBaseOperations + Send + Sync + Clone + 'static + std::fmt::Display,
{
    pub fn new(
        metrics: Arc<MetricBuffer>,
        node: Node,
        subscriber: Subscriber,
        listen: (Ipv4Addr, u16),
        origin: String,
    ) -> Self {
        let memory = Arc::new(RwLock::new(Connections::default()));
        Self {
            memory,
            metrics,
            node,
            subscriber,
            listen: listen.0,
            port: listen.1,
            origin,
        }
    }

    pub async fn start_server(&self) {
        let health_check = warp::path("health-check").map(|| "Server OK");

        let cors = warp::cors()
            .allow_origin(self.origin.as_str())
            .allow_methods(vec!["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"])
            .allow_headers(vec!["Authorization", "Content-Type"])
            .max_age(86400)
            .build();

        tracing::debug!("CORS: {:?}", cors.clone());

        let memory = self.memory.clone();

        let auth_route = warp::post()
            .and(warp::path("auth"))
            .and(warp::body::json::<request::Auth>())
            .and(warp::any().map(move || memory.clone()))
            .and_then(auth_handler);

        let routes = health_check.or(auth_route);

        warp::serve(routes.with(cors))
            .run(SocketAddr::new(IpAddr::V4(self.listen), self.port))
            .await;
    }
}

pub async fn run(settings: ServiceSettings) -> Result<()> {
    let mut tasks: Vec<JoinHandle<()>> = vec![];
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    let node_config = NodeConfig::from_raw(settings.node.clone());
    let node = Node::new(node_config?, None, None);

    let topic_init: Topic = settings.node.uuid.into();

    let subscriber = Subscriber::new(
        &settings.service.updates_endpoint_zmq,
        vec![topic_init, Topic::Auth],
    );

    let metrics = MetricBuffer {
        batch: parking_lot::Mutex::new(Vec::new()),
        publisher: Publisher::connect(&settings.metrics.publisher).await?,
    };

    let auth_service = Arc::new(Service::<Connection>::new(
        Arc::new(metrics),
        node,
        subscriber?,
        (settings.service.listen, settings.service.port),
        settings.service.origin.clone(),
    ));

    let snapshot_manager = SnapshotManager::new(
        settings.clone().service.snapshot_path,
        auth_service.memory.clone(),
    );

    let snapshot_timestamp = if Path::new(&snapshot_manager.snapshot_path).exists() {
        match snapshot_manager.load_snapshot().await {
            Ok(Some(timestamp)) => {
                let count = snapshot_manager.len().await;
                tracing::info!(
                    "Loaded {} auth connections from snapshot with ts {}",
                    count,
                    timestamp,
                );
                Some(timestamp)
            }
            Ok(None) => {
                tracing::error!("Snapshot file exists but couldn't be loaded");
                panic!("napshot file exists but couldn't be loaded")
            }
            Err(e) => {
                tracing::error!("Failed to load snapshot: {}", e);
                tracing::info!("Starting fresh due to snapshot load error");
                None
            }
        }
    } else {
        tracing::info!("No snapshot found, starting fresh");
        None
    };

    let snapshot_manager = snapshot_manager.clone();
    let snapshot_handle = tokio::spawn(async move {
        tracing::info!(
            "Running snapshot task, interval {}",
            settings.service.snapshot_interval
        );

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(
            settings.service.snapshot_interval,
        ));
        loop {
            interval.tick().await;
            if let Err(e) = snapshot_manager.create_snapshot().await {
                tracing::error!("Failed to create snapshot: {}", e);
            } else {
                let len = snapshot_manager.len().await;
                tracing::debug!("Auth snapshot saved successfully; {} connections", len);
            }
        }
    });

    tasks.push(snapshot_handle);

    {
        tracing::info!("ZMQ listener starting...");

        let zmq_task = tokio::spawn({
            let auth_service = auth_service.clone();
            let mut shutdown = shutdown_tx.subscribe();
            async move {
                tokio::select! {
                    _ = auth_service.run_subscriber() => {},
                    _ = shutdown.recv() => {},
                }
            }
        });
        tasks.push(zmq_task);

        sleep(std::time::Duration::from_millis(500)).await;
    };

    {
        let settings = settings.clone();
        let auth_service = auth_service.clone();

        loop {
            let api_token = settings.api.token.clone();
            match auth_service
                .get_connections(
                    settings.api.endpoint.clone(),
                    api_token,
                    Tag::Hysteria2,
                    snapshot_timestamp,
                )
                .await
            {
                Ok(_) => {
                    break;
                }
                Err(e) => {
                    tracing::warn!("Api not available {} retrying...", e);
                    sleep(Duration::from_secs(10)).await;
                }
            }
        }
    };

    {
        let mut shutdown = shutdown_tx.subscribe();
        let auth_service = auth_service.clone();

        let service_handle = tokio::spawn(async move {
            tokio::select! {
                _ = auth_service.start_server() => {},
                _ = shutdown.recv() => {},
            }
        });
        tasks.push(service_handle);
    };

    tracing::info!("Running metrics task");

    let service_to = auth_service.clone();
    let metrics_handle = tokio::spawn({
        let mut shutdown = shutdown_tx.subscribe();
        async move {
            loop {
                tokio::select! {
                    _ = sleep(Duration::from_secs(settings.metrics.interval)) => {
                        service_to.collect_metrics().await;
                    },
                    _ = shutdown.recv() => break,
                }
            }
        }
    });

    let service_to = auth_service.clone();
    tracing::info!("Running flush metrics task");
    let metrics_flush_handle = tokio::spawn({
        let mut shutdown = shutdown_tx.subscribe();
        async move {
            loop {
                tokio::select! {
                    _ = sleep(Duration::from_secs(settings.metrics.interval + 2)) => {
                        service_to.metrics.flush_to_zmq().await;
                    },
                    _ = shutdown.recv() => break,
                }
            }
        }
    });

    tasks.push(metrics_handle);
    tasks.push(metrics_flush_handle);

    wait_all_tasks_or_ctrlc(tasks, shutdown_tx).await;
    Ok(())
}

async fn wait_all_tasks_or_ctrlc(tasks: Vec<JoinHandle<()>>, shutdown_tx: broadcast::Sender<()>) {
    tokio::select! {
        _ = async {
            for (i, task) in tasks.into_iter().enumerate() {
                match task.await {
                    Ok(_) => {
                        tracing::info!("Task {i} completed successfully");
                    }

                    Err(e) => {
                        tracing::error!("Task {i} panicked: {:?}", e);
                        let _ = shutdown_tx.send(());
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        std::process::exit(1);
                    }
                }
            }
        } => {}
        _ = signal::ctrl_c() => {
            tracing::info!("🛑 Ctrl+C received. Shutting down...");
            let _ = shutdown_tx.send(());
            tokio::time::sleep(Duration::from_secs(5)).await;
            std::process::exit(0);
        }
    }
}
