use std::path::Path;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::broadcast;
#[cfg(feature = "xray")]
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio::time::Duration;

use tracing::{debug, error, info, warn};

#[cfg(feature = "xray")]
use fcore::{XrayClient, XrayHandlerClient, XraySettings, XrayStatsClient};

#[cfg(feature = "wireguard")]
use fcore::{WgApi, WireguardServerConfig, WireguardSettings};

#[cfg(feature = "amnezia-wg")]
use fcore::{AmneziaWgServerConfig, AmneziaWgSettings, AwgInterface, Error};

use fcore::{
    utils::measure_time, BaseConnection as Connection, ConnectionBaseOperations, Connections,
    MetricBuffer, Node as MemNode, Publisher, Result, SnapshotManager, Subscriber, Tag, Topic,
};

use fcore::{H2Settings, Hysteria2Settings, MtprotoSettings, NodeConfig, Settings};

use super::config::ServiceSettings;
use super::http::ApiRequests;
#[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
use super::snapshot::SnapshotRestore;
use super::tasks::Tasks;

pub struct Node<C>
where
    C: ConnectionBaseOperations + Send + Sync + Clone + 'static,
{
    pub memory: Arc<RwLock<Connections<C>>>,
    pub node: MemNode,
    pub metrics: Arc<MetricBuffer>,
    pub subscriber: Subscriber,
    #[cfg(feature = "xray")]
    pub stats_client: Option<Arc<Mutex<XrayStatsClient>>>,
    #[cfg(feature = "xray")]
    pub handler_client: Option<Arc<Mutex<XrayHandlerClient>>>,
    #[cfg(feature = "wireguard")]
    pub wg_client: Option<WgApi>,
    #[cfg(feature = "amnezia-wg")]
    pub awg_client: Option<AwgInterface>,
}

impl<C> Node<C>
where
    C: ConnectionBaseOperations + Send + Sync + Clone + 'static,
{
    pub fn new(
        node: MemNode,
        subscriber: Subscriber,
        metrics: Arc<MetricBuffer>,
        #[cfg(feature = "xray")] stats_client: Option<Arc<Mutex<XrayStatsClient>>>,
        #[cfg(feature = "xray")] handler_client: Option<Arc<Mutex<XrayHandlerClient>>>,
        #[cfg(feature = "wireguard")] wg_client: Option<WgApi>,
        #[cfg(feature = "amnezia-wg")] awg_client: Option<AwgInterface>,
    ) -> Self {
        let memory = Arc::new(RwLock::new(Connections::default()));
        Self {
            memory,
            node,
            metrics,
            subscriber,
            #[cfg(feature = "xray")]
            stats_client,
            #[cfg(feature = "xray")]
            handler_client,
            #[cfg(feature = "wireguard")]
            wg_client,
            #[cfg(feature = "amnezia-wg")]
            awg_client,
        }
    }
}

pub async fn run(settings: ServiceSettings) -> Result<()> {
    let mut tasks: Vec<JoinHandle<()>> = vec![];
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // Init Xray
    #[cfg(feature = "xray")]
    let (xray_config, stats_client, handler_client) = if settings.xray.enabled {
        let config = match XraySettings::from_file(&settings.xray.path) {
            Ok(config) => {
                info!(
                    "Xray Config: Successfully read Xray config file: {:?}",
                    config
                );

                if let Err(e) = config.validate() {
                    panic!("Xray Config:: Error reading JSON file: {}", e);
                }
                config
            }
            Err(e) => {
                panic!("Xray Config:: Error reading JSON file: {}", e);
            }
        };

        let xray_api_endpoint = format!("http://{}", config.api.listen.clone());

        let stats_client = Arc::new(Mutex::new(XrayStatsClient::new(&xray_api_endpoint).await?));
        let handler_client = Arc::new(Mutex::new(
            XrayHandlerClient::new(&xray_api_endpoint).await?,
        ));

        (Some(config), Some(stats_client), Some(handler_client))
    } else {
        (None, None, None)
    };

    // Init Wireguard
    #[cfg(feature = "wireguard")]
    let (wg_client, wg_config) = if settings.wg.enabled {
        let row_config = WireguardServerConfig::from_file(&settings.wg.path)?;
        let wg: WireguardSettings = row_config.try_into()?;
        debug!("{:?}", wg);
        let client = match WgApi::new(&wg.interface) {
            Ok(c) => c,
            Err(e) => panic!("Cannot create WG client: {}", e),
        };
        if let Err(e) = client.validate() {
            panic!("Cannot validate WG client: {}", e);
        }
        (Some(client), Some(wg))
    } else {
        (None, None)
    };

    #[cfg(feature = "amnezia-wg")]
    let (awg_client, awg_config) = if settings.awg.enabled {
        let raw_config = AmneziaWgServerConfig::from_file(&settings.awg.path)?;
        let awg: AmneziaWgSettings = raw_config.try_into()?;

        debug!("{:?}", awg);

        let client = AwgInterface::connect(awg.interface.interface.clone())
            .map_err(|e| Error::Custom(format!("Cannot create AWG client: {}", e)))?;

        // optional: validate via real netlink call
        let _device = client
            .get_device()
            .map_err(|e| Error::Custom(format!("Cannot validate AWG client: {}", e)))?;

        if let Some(ref obf) = awg.obfuscation {
            client
                .set_obfuscation_params(obf)
                .map_err(|e| Error::Custom(format!("Cannot configure AWG obfuscation: {}", e)))?;
        }

        (Some(client), Some(awg))
    } else {
        (None, None)
    };

    // Init Hysteria2
    let h2_config = if settings.h2.enabled {
        match Hysteria2Settings::from_file(&settings.h2.path) {
            Ok(cfg) => {
                if let Err(e) = cfg.validate() {
                    error!("Hysteria2 config validation failed: {}", e);
                    panic!("Hysteria2 config: {}", e);
                } else {
                    match H2Settings::try_from(cfg) {
                        Ok(settings) => Some(settings),
                        Err(e) => {
                            error!("Hysteria2 validation error: {}", e);
                            panic!("Hysteria2 config: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                error!("Failed to load Hysteria2 config: {}", e);
                panic!("Hysteria2 config: {}", e);
            }
        }
    } else {
        None
    };

    // Init Mtproto
    let mtproto_config = if settings.mtproto.enabled {
        Some(MtprotoSettings::from_file(&settings.mtproto.path))
    } else {
        None
    };

    let node_config = NodeConfig::from_raw(settings.node.clone());

    let node = MemNode::new(
        node_config?,
        #[cfg(feature = "xray")]
        xray_config,
        #[cfg(feature = "wireguard")]
        wg_config,
        #[cfg(feature = "amnezia-wg")]
        awg_config,
        h2_config,
        mtproto_config,
    );

    let topic_init: Topic = settings.node.env.clone().into();
    let topic_updates: Topic = settings.node.uuid.into();

    let topics = vec![topic_updates, topic_init];

    tracing::debug!("Topics to connect {:?}", topics);
    let subscriber = Subscriber::new(&settings.service.updates_endpoint_zmq, topics)?;
    let metric_publisher = Publisher::connect(&settings.metrics.publisher).await?;

    let metrics = MetricBuffer {
        batch: parking_lot::Mutex::new(Vec::new()),
        publisher: metric_publisher,
    };

    let node = Arc::new(Node::<Connection>::new(
        node.clone(),
        subscriber,
        Arc::new(metrics),
        #[cfg(feature = "xray")]
        stats_client.clone(),
        #[cfg(feature = "xray")]
        handler_client.clone(),
        #[cfg(feature = "wireguard")]
        wg_client.clone(),
        #[cfg(feature = "amnezia-wg")]
        awg_client.clone(),
    ));

    let snapshot_path = settings.service.snapshot_path.clone();
    let snapshot_manager = SnapshotManager::new(snapshot_path, node.memory.clone());

    let snapshot_timestamp = if Path::new(&snapshot_manager.snapshot_path).exists() {
        match snapshot_manager.load_snapshot().await {
            Ok(Some(timestamp)) => {
                #[cfg(feature = "wireguard")]
                if let Err(e) = snapshot_manager.restore_wg_connections(wg_client).await {
                    error!("Couldn't restore connections from memory, {}", e);
                }

                #[cfg(feature = "amnezia-wg")]
                if let Err(e) = snapshot_manager.restore_awg_connections(awg_client.clone()).await {
                    error!("Couldn't restore AmneziaWG connections from memory, {}", e);
                }

                #[cfg(feature = "xray")]
                if let Err(e) = snapshot_manager
                    .restore_xray_connections(handler_client)
                    .await
                {
                    error!("Couldn't restore connections from memory, {}", e);
                }

                let count = snapshot_manager.len().await;
                info!(
                    "Loaded {} connections from snapshot with ts  {}",
                    count, timestamp,
                );
                Some(timestamp)
            }
            Ok(None) => {
                warn!("Snapshot file exists but couldn't be loaded");
                None
            }
            Err(e) => {
                error!("Failed to load snapshot: {}", e);
                info!("Starting fresh due to snapshot load error");
                None
            }
        }
    } else {
        warn!("No snapshot found, starting fresh");
        None
    };
    {
        tokio::spawn(async move {
            info!(
                "Running snapshot task, interval {}",
                settings.service.snapshot_interval
            );

            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                settings.service.snapshot_interval,
            ));

            loop {
                interval.tick().await;
                if let Err(e) = measure_time(snapshot_manager.create_snapshot(), "Snapshot").await {
                    error!("Failed to create snapshot: {}", e);
                } else {
                    let count = snapshot_manager.len().await;
                    debug!(
                        "Connections snapshot saved successfully; {} Connections",
                        count
                    );
                }
            }
        });

        {
            info!("ZMQ listener starting...");
            let zmq_task = tokio::spawn({
                let node = node.clone();
                let mut shutdown = shutdown_tx.subscribe();
                async move {
                    tokio::select! {
                        _ = node.run_subscriber() => {},
                        _ = shutdown.recv() => {},
                    }
                }
            });
            tasks.push(zmq_task);

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            {
                let settings = settings.clone();
                let node = node.clone();

                loop {
                    match node
                        .register_node(settings.api.endpoint.clone(), settings.api.token.clone())
                        .await
                    {
                        Ok(_) => {
                            let tags: Vec<_> = node
                                .node
                                .inbounds
                                .keys()
                                .filter(|k| !matches!(k, Tag::Hysteria2)) // Hysteria2 uses external auth provider
                                .filter(|k| !matches!(k, Tag::Mtproto)) // Mtproto doesn't support auth provider
                                .collect();

                            for tag in tags {
                                node.sync_connections(
                                    settings.api.endpoint.clone(),
                                    settings.api.token.clone(),
                                    *tag,
                                    snapshot_timestamp,
                                )
                                .await?
                            }
                            break;
                        }
                        Err(e) => {
                            warn!("API unavailable, {} retrying... ", e);
                            sleep(Duration::from_secs(10)).await;
                        }
                    }
                }
            };
        };
    }

    info!("Running metrics task");

    let metrics_handle: JoinHandle<()> = tokio::spawn({
        let node = node.clone();
        let mut shutdown = shutdown_tx.subscribe();
        async move {
            loop {
                tokio::select! {
                    _ = sleep(Duration::from_secs(settings.metrics.interval)) => {
                         node.collect_metrics().await;

                    },
                    _ = shutdown.recv() => {
                        info!("🛑 Metrics task received shutdown");
                        break;
                    },
                }
            }
        }
    });

    info!("Running flush metrics task");
    let metrics_flush_handle: JoinHandle<()> = tokio::spawn({
        let node = node.clone();
        let mut shutdown = shutdown_tx.subscribe();
        async move {
            loop {
                tokio::select! {
                    _ = sleep(Duration::from_secs(settings.metrics.interval+3)) => {
                         node.metrics.flush_to_zmq().await;

                    },
                    _ = shutdown.recv() => {
                        info!("🛑 Metrics flush task received shutdown");
                        break;
                    },
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
                        info!("Task {i} completed successfully");
                    }

                    Err(e) => {
                        error!("Task {i} panicked: {:?}", e);
                        let _ = shutdown_tx.send(());
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        std::process::exit(1);
                    }
                }
            }
        } => {}
        _ = signal::ctrl_c() => {
            info!("🛑 Ctrl+C received. Shutting down...");
            let _ = shutdown_tx.send(());
            tokio::time::sleep(Duration::from_secs(5)).await;
            std::process::exit(0);
        }
    }
}
