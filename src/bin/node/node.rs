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
use super::http::{ApiRequests, SyncOutcome};
#[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
use super::snapshot::SnapshotRestore;
use super::snapshot_sync::FullSyncTracker;
use super::tasks::Tasks;

pub struct Node<C>
where
    C: ConnectionBaseOperations + Send + Sync + Clone + 'static,
{
    pub memory: Arc<RwLock<Connections<C>>>,
    /// Tunnels of the peers plane, keyed by public key. The same map the
    /// snapshot persists, so there is one copy of the truth.
    pub peers: Arc<RwLock<fcore::PeerMirror>>,
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
    #[cfg(feature = "amnezia-wg")]
    pub awg_mobile_client: Option<AwgInterface>,
    /// Peers plane interfaces: members are owned by the api, the node only
    /// programs them.
    #[cfg(feature = "amnezia-wg")]
    pub awg_peers_client: Option<AwgInterface>,
    #[cfg(feature = "wireguard")]
    pub wg_peers_client: Option<WgApi>,
    /// Wakes the write-through snapshot task after an applied message.
    pub snapshot_wake: Arc<tokio::sync::Notify>,
    /// Tags whose authoritative full list is expected right now, with the ids
    /// seen so far and the count the api promised. Prune is released only when
    /// both agree; a partial batch or a failed request must never remove peers.
    pub full_sync: Arc<tokio::sync::Mutex<FullSyncTracker>>,
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
        #[cfg(feature = "amnezia-wg")] awg_mobile_client: Option<AwgInterface>,
        #[cfg(feature = "amnezia-wg")] awg_peers_client: Option<AwgInterface>,
        #[cfg(feature = "wireguard")] wg_peers_client: Option<WgApi>,
    ) -> Self {
        let memory = Arc::new(RwLock::new(Connections::default()));
        let peers = Arc::new(RwLock::new(std::collections::HashMap::new()));
        Self {
            memory,
            peers,
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
            #[cfg(feature = "amnezia-wg")]
            awg_mobile_client,
            #[cfg(feature = "amnezia-wg")]
            awg_peers_client,
            #[cfg(feature = "wireguard")]
            wg_peers_client,
            snapshot_wake: Arc::new(tokio::sync::Notify::new()),
            full_sync: Arc::new(tokio::sync::Mutex::new(FullSyncTracker::default())),
        }
    }
}

pub async fn run(settings: ServiceSettings, peers_resync: bool) -> Result<()> {
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
        let mut wg: WireguardSettings = row_config.try_into()?;
        wg.keepalive = settings.wg.keepalive;
        // Никаких `{:?}`: в WireguardSettings лежит WgKeys с приватным ключом.
        debug!(
            "wireguard: iface {}, {}:{}, peers из файла",
            wg.interface, wg.address, wg.port
        );
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
        let mut awg: AmneziaWgSettings = raw_config.try_into()?;
        awg.keepalive = settings.awg.keepalive;

        debug!(
            "amneziawg: iface {}, {:?}:{}, mtu {:?}",
            awg.interface.interface,
            awg.interface.address,
            awg.interface.listen_port,
            awg.interface.mtu
        );

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

    // Second AmneziaWG interface (e.g. awg1) with the dedicated mobile pool.
    #[cfg(feature = "amnezia-wg")]
    let (awg_mobile_client, awg_mobile_config) = if settings.awg_mobile.enabled {
        let raw_config = AmneziaWgServerConfig::from_file(&settings.awg_mobile.path)?;
        let mut awg: AmneziaWgSettings = raw_config.try_into()?;
        awg.keepalive = settings.awg_mobile.keepalive;

        debug!("{:?}", awg);

        let client = AwgInterface::connect(awg.interface.interface.clone())
            .map_err(|e| Error::Custom(format!("Cannot create AWG mobile client: {}", e)))?;

        // optional: validate via real netlink call
        let _device = client
            .get_device()
            .map_err(|e| Error::Custom(format!("Cannot validate AWG mobile client: {}", e)))?;

        if let Some(ref obf) = awg.obfuscation {
            client.set_obfuscation_params(obf).map_err(|e| {
                Error::Custom(format!("Cannot configure AWG mobile obfuscation: {}", e))
            })?;
        }

        (Some(client), Some(awg))
    } else {
        (None, None)
    };

    // Peers plane: a separate interface whose members come from the api.
    // The node only connects; bringing the device up is the operator's job
    // (awg-quick / wg-quick), and a missing device is a hard error because a
    // node that silently skips peers would strand every tunnel of its env.
    #[cfg(feature = "amnezia-wg")]
    let (awg_peers_client, awg_peers_config) = if settings.awg_peers.enabled {
        let raw_config = AmneziaWgServerConfig::from_file(&settings.awg_peers.path)?;
        let mut awg: AmneziaWgSettings = raw_config.try_into()?;
        awg.keepalive = settings.awg_peers.keepalive;

        debug!(
            "amneziawg peers: iface {}, {:?}:{}, mtu {:?}",
            awg.interface.interface,
            awg.interface.address,
            awg.interface.listen_port,
            awg.interface.mtu
        );

        let client = AwgInterface::connect(awg.interface.interface.clone()).map_err(|e| {
            Error::Custom(format!(
                "Cannot connect to AWG peers interface '{}': {}. Bring it up with \
                 `awg-quick up <iface>` first.",
                awg.interface.interface, e
            ))
        })?;
        let _device = client.get_device().map_err(|e| {
            Error::Custom(format!(
                "AWG peers interface '{}' is not usable: {}. Bring it up with \
                 `awg-quick up <iface>` first.",
                awg.interface.interface, e
            ))
        })?;

        if let Some(ref obf) = awg.obfuscation {
            client.set_obfuscation_params(obf).map_err(|e| {
                Error::Custom(format!("Cannot configure AWG peers obfuscation: {}", e))
            })?;
        }

        (Some(client), Some(awg))
    } else {
        (None, None)
    };

    #[cfg(feature = "wireguard")]
    let (wg_peers_client, wg_peers_config) = if settings.wg_peers.enabled {
        let raw_config = WireguardServerConfig::from_file(&settings.wg_peers.path)?;
        let mut wg: WireguardSettings = raw_config.try_into()?;
        wg.keepalive = settings.wg_peers.keepalive;
        debug!(
            "wireguard peers: iface {}, {}:{}",
            wg.interface, wg.address, wg.port
        );
        let client = WgApi::new(&wg.interface).map_err(|e| {
            Error::Custom(format!(
                "Cannot connect to WG peers interface '{}': {}",
                wg.interface, e
            ))
        })?;
        client.validate().map_err(|e| {
            Error::Custom(format!(
                "WG peers interface '{}' is not usable: {}. Bring it up with \
                 `wg-quick up <iface>` first.",
                wg.interface, e
            ))
        })?;
        (Some(client), Some(wg))
    } else {
        (None, None)
    };

    // Init Hysteria2. fnode never starts it: the process is a separate
    // systemd unit, and its own config file is the single source of truth for
    // what gets announced (port from `listen`, name from `host`/`acme.domains`).
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
        #[cfg(feature = "amnezia-wg")]
        awg_mobile_config,
        #[cfg(feature = "amnezia-wg")]
        awg_peers_config,
        #[cfg(feature = "wireguard")]
        wg_peers_config,
        h2_config,
        mtproto_config,
    );

    let topic_init: Topic = settings.node.env.clone().into();
    let topic_updates: Topic = settings.node.uuid.into();

    // Peer topics are separate from the connection stream. The live stream is
    // scoped to this node's pool (`peers-update-<env>-<pool>`): the same device
    // key holds one address per pool, and a node must only hear about the pool
    // it serves. A config without a pool keeps the env-wide topic and the
    // env-wide snapshot — that is the pre-pool generation.
    let mut topics = vec![topic_updates, topic_init];
    topics.push(Topic::PeersUpdates(
        settings.node.env.clone(),
        settings.node.pool.clone(),
    ));
    topics.push(Topic::PeersInit(settings.node.uuid));

    tracing::debug!("Topics to connect {:?}", topics);
    let subscriber = Subscriber::new(&settings.service.updates_endpoint_zmq, topics)?;
    let metric_publisher = Publisher::connect(&settings.metrics.publisher).await?;

    let metrics = MetricBuffer {
        batch: parking_lot::Mutex::new(Vec::new()),
        entries: parking_lot::Mutex::new(Vec::new()),
        entry_seen: parking_lot::Mutex::new(std::collections::HashMap::new()),
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
        #[cfg(feature = "amnezia-wg")]
        awg_mobile_client.clone(),
        #[cfg(feature = "amnezia-wg")]
        awg_peers_client.clone(),
        #[cfg(feature = "wireguard")]
        wg_peers_client.clone(),
    ));

    let metrics_endpoint = settings.metrics.endpoint.clone();
    let metrics_enabled = settings.metrics.enabled;

    let snapshot_path = settings.service.snapshot_path.clone();
    let snapshot_manager = SnapshotManager::new(snapshot_path, node.memory.clone())
        .with_identity(fcore::SnapshotIdentity {
            hostname: settings.node.hostname.clone().unwrap_or_default(),
            env: settings.node.env.to_string(),
            pool: settings.node.pool.clone(),
        })
        .with_peers(node.peers.clone())
        .with_wake(node.snapshot_wake.clone());

    let metrics_server_handle = if metrics_enabled {
        let addr = metrics_endpoint
            .parse::<std::net::SocketAddr>()
            .expect("Invalid metrics.endpoint address");
        let node = node.clone();
        let healthcheck_token = settings.metrics.healthcheck_token.clone();
        let mut shutdown = shutdown_tx.subscribe();
        Some(tokio::spawn(async move {
            info!("Node metrics HTTP server listening on {}", addr);
            let routes = crate::metrics_http::routes(node, healthcheck_token);
            let (_, server) = warp::serve(routes).bind_with_graceful_shutdown(addr, async move {
                let _ = shutdown.recv().await;
            });
            server.await;
            info!("Node metrics HTTP server shut down");
        }))
    } else {
        None
    };
    if let Some(h) = metrics_server_handle {
        tasks.push(h);
    }

    if peers_resync {
        // `--peers-resync`: ignore the local file for this start. The api's
        // `peers-init` is authoritative and the interface is reconciled against
        // the mirror, so the node rebuilds its peer set from the api instead of
        // trusting a snapshot that may be exactly what is wrong.
        info!("--peers-resync: snapshot ignored, peer set rebuilt from the api");
    } else if Path::new(&snapshot_manager.snapshot_path).exists() {
        match snapshot_manager.load_snapshot().await {
            Ok(Some(meta)) => {
                // The snapshot is only worth applying if its per-tag counts
                // still match what the api would send: a file that no longer
                // describes reality used to be applied as-is, and the node then
                // served a subset of the peers until somebody intervened.
                let mut accepted = true;
                if settings.snapshot.validate_on_start {
                    let tags: Vec<Tag> = node.node.inbounds.keys().copied().collect();
                    match node
                        .connection_counts(
                            settings.api.endpoint.clone(),
                            settings.api.token.clone(),
                            node.node.env.clone(),
                            tags,
                        )
                        .await
                    {
                        Ok(api_counts) => {
                            let mut bad = Vec::new();
                            for (tag, api_n) in &api_counts {
                                let local = meta.counts.get(tag).copied().unwrap_or(0);
                                let divergence = divergence_ratio(local, *api_n);
                                node.metrics.push(
                                    node.node.uuid,
                                    "snapshot.divergence_ratio",
                                    divergence,
                                    std::collections::BTreeMap::from([(
                                        "tag".to_string(),
                                        tag.clone(),
                                    )]),
                                );
                                if settings.snapshot.is_divergent(local, *api_n) {
                                    bad.push((tag.clone(), local, *api_n));
                                }
                            }
                            if bad.is_empty() {
                                info!(
                                    "snapshot ok: {} tag(s), {} connections",
                                    api_counts.len(),
                                    meta.counts.values().sum::<u64>()
                                );
                            } else {
                                warn!("snapshot rejected: {:?} -> full resync from api", bad);
                                accepted = false;
                            }
                        }
                        Err(e) => warn!(
                            "snapshot check skipped (api unavailable: {}), applying as is",
                            e
                        ),
                    }
                }

                if accepted {
                    #[cfg(feature = "wireguard")]
                    if let Err(e) = snapshot_manager.restore_wg_connections(wg_client).await {
                        error!("Couldn't restore connections from memory, {}", e);
                    }

                    #[cfg(feature = "amnezia-wg")]
                    if let Err(e) = snapshot_manager
                        .restore_awg_connections(awg_client.clone(), awg_mobile_client.clone())
                        .await
                    {
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
                        count, meta.timestamp,
                    );
                } else {
                    // Move the file aside so the next start does not repeat the
                    // check, and start from an empty memory: the api list is
                    // authoritative and arrives with the first full sync.
                    let stale = format!(
                        "{}.stale-{}",
                        snapshot_manager.snapshot_path,
                        chrono::Utc::now().timestamp()
                    );
                    if let Err(e) = std::fs::rename(&snapshot_manager.snapshot_path, &stale) {
                        error!("cannot move stale snapshot aside: {}", e);
                    }
                    node.memory.write().await.0.clear();
                }
            }
            Ok(None) => {
                warn!("Snapshot file exists but couldn't be loaded");
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("identity mismatch") {
                    // Deliberate hard stop: a foreign snapshot is an operator
                    // error, and silently serving it is worse than not starting.
                    error!("{}", msg);
                    eprintln!("FATAL: {}", msg);
                    std::process::exit(1);
                }
                error!("Failed to load snapshot: {}", e);
                info!("Starting fresh due to snapshot load error");
            }
        }
    } else {
        warn!("No snapshot found, starting fresh");
    };

    // The interfaces are up and the snapshot (if any) is applied. Reconcile the
    // whole interface against the mirror now: a peer that the file did not
    // carry, or that a missed message never applied, must not need a manual
    // `awg-quick down/up` to appear. `peers-init` reconciles again once the api
    // sends its authoritative set.
    node.reconcile_peers_with_mirror().await;

    // Write-through: every applied Create/Update/Delete persists immediately,
    // so a restart in the same second cannot resurrect a deleted connection.
    {
        let manager = snapshot_manager.clone();
        let wake = node.snapshot_wake.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                wake.notified().await;
                // Debounce a burst: one write captures the whole batch.
                tokio::time::sleep(Duration::from_millis(250)).await;
                let started = std::time::Instant::now();
                match measure_time(manager.create_snapshot(), "Snapshot (write-through)").await {
                    Ok(()) => {
                        debug!("write-through snapshot written in {:?}", started.elapsed());
                    }
                    Err(e) => {
                        error!("write-through snapshot failed: {}", e);
                    }
                }
            }
        }));
    }

    // Hourly drift check: compare per-tag counts with the api and tell it when
    // they diverge. Nothing is reset here on purpose — a short api outage must
    // not wipe a working node.
    if settings.snapshot.drift_check_sec > 0 {
        let node = node.clone();
        let endpoint = settings.api.endpoint.clone();
        let token = settings.api.token.clone();
        let cfg = settings.snapshot.clone();
        tasks.push(tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_secs(cfg.drift_check_sec.max(60)));
            interval.tick().await;
            loop {
                interval.tick().await;
                let by_name: std::collections::HashMap<String, Tag> = node
                    .node
                    .inbounds
                    .keys()
                    .map(|tag| (format!("{tag:?}"), *tag))
                    .collect();
                let tags: Vec<Tag> = by_name.values().copied().collect();
                let api_counts = match node
                    .connection_counts(endpoint.clone(), token.clone(), node.node.env.clone(), tags)
                    .await
                {
                    Ok(counts) => counts,
                    Err(e) => {
                        warn!("drift check skipped: {}", e);
                        continue;
                    }
                };
                let local_counts = {
                    let mem = node.memory.read().await;
                    let mut out: std::collections::BTreeMap<String, u64> =
                        std::collections::BTreeMap::new();
                    for (_, conn) in mem.iter() {
                        *out.entry(format!("{:?}", conn.get_proto().proto()))
                            .or_insert(0) += 1;
                    }
                    out
                };
                for (tag_name, api_n) in api_counts {
                    let local = local_counts.get(&tag_name).copied().unwrap_or(0);
                    if !cfg.is_divergent(local, api_n) {
                        continue;
                    }
                    warn!(
                        "snapshot drift: tag={} local={} api={}",
                        tag_name, local, api_n
                    );
                    node.metrics.push(
                        node.node.uuid,
                        "snapshot.divergence_ratio",
                        divergence_ratio(local, api_n),
                        std::collections::BTreeMap::from([("tag".to_string(), tag_name.clone())]),
                    );
                    match by_name.get(&tag_name) {
                        Some(tag) => {
                            if let Err(e) = node
                                .report_snapshot_drift(
                                    endpoint.clone(),
                                    token.clone(),
                                    *tag,
                                    local,
                                    api_n,
                                )
                                .await
                            {
                                warn!("cannot report snapshot drift: {}", e);
                            }
                        }
                        None => warn!("drift for unknown tag {}", tag_name),
                    }
                }
            }
        }));
    }

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

            // On-demand resync: `systemctl reload fnode` (SIGHUP) re-registers,
            // which makes the api publish the authoritative peer set again and
            // the node reconcile the whole interface against it. A node whose
            // snapshot was lost is repaired by a reload, not by SQL.
            #[cfg(unix)]
            {
                let node = node.clone();
                let endpoint = settings.api.endpoint.clone();
                let token = settings.api.token.clone();
                let mut shutdown = shutdown_tx.subscribe();
                tasks.push(tokio::spawn(async move {
                    let mut hup = match tokio::signal::unix::signal(
                        tokio::signal::unix::SignalKind::hangup(),
                    ) {
                        Ok(signal) => signal,
                        Err(e) => {
                            warn!("SIGHUP handler disabled: {}", e);
                            return;
                        }
                    };
                    // PUB/SUB drops a message published before the subscription
                    // is established, and the node's only copy of the peer set
                    // arrives that way. One delayed re-register covers a slow
                    // start: it is idempotent and the api answers with the
                    // authoritative set again.
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                    if let Err(e) = node.register_node(endpoint.clone(), token.clone()).await {
                        warn!("peers: second registration failed: {}", e);
                    } else {
                        node.reconcile_peers_with_mirror().await;
                    }

                    loop {
                        tokio::select! {
                            _ = hup.recv() => {
                                info!("SIGHUP: re-registering for the authoritative peer set");
                                match node
                                    .register_node(endpoint.clone(), token.clone())
                                    .await
                                {
                                    Ok(_) => {
                                        info!("SIGHUP: re-registered");
                                        node.reconcile_peers_with_mirror().await;
                                    }
                                    Err(e) => warn!("SIGHUP: re-register failed: {}", e),
                                }
                            }
                            _ = shutdown.recv() => break,
                        }
                    }
                }));
            }

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
                            if let Some(config_path) = std::env::args().nth(1) {
                                if adopt_private_env(&config_path, &settings.node.env) {
                                    let _ = std::fs::remove_file(&settings.service.snapshot_path);
                                    std::process::exit(1);
                                }
                            }
                            let tags: Vec<_> = node
                                .node
                                .inbounds
                                .keys()
                                .filter(|k| !matches!(k, Tag::Hysteria2)) // Hysteria2 uses external auth provider
                                .filter(|k| !matches!(k, Tag::Mtproto)) // Mtproto doesn't support auth provider
                                .collect();

                            for tag in tags {
                                // The first sync of this process is always a
                                // full one: the tracker releases prune only
                                // after the api's promised count arrives.
                                node.full_sync.lock().await.arm(*tag);
                                match node
                                    .sync_connections(
                                        settings.api.endpoint.clone(),
                                        settings.api.token.clone(),
                                        *tag,
                                        None,
                                    )
                                    .await
                                {
                                    Ok(SyncOutcome::Applied { count }) => {
                                        let keep = node.full_sync.lock().await.finish(*tag, count);
                                        match keep {
                                            Some(keep) => {
                                                if let Err(e) = node.prune_tag(*tag, &keep).await {
                                                    warn!(
                                                        "full sync prune failed for {:?}: {}",
                                                        tag, e
                                                    );
                                                }
                                            }
                                            None => debug!(
                                                "full sync waiting for {:?}: expected {} ids",
                                                tag, count
                                            ),
                                        }
                                    }
                                    Ok(SyncOutcome::EmptyFull) => {
                                        let keep = node.full_sync.lock().await.finish(*tag, 0);
                                        if let Some(keep) = keep {
                                            let _ = node.prune_tag(*tag, &keep).await;
                                        }
                                    }
                                    Ok(SyncOutcome::NotModified) => {
                                        node.full_sync.lock().await.cancel(*tag);
                                        warn!(
                                            "full sync for {:?} returned 304; keeping local state",
                                            tag
                                        );
                                    }
                                    Err(e) => {
                                        node.full_sync.lock().await.cancel(*tag);
                                        return Err(e);
                                    }
                                }
                            }
                            break;
                        }
                        Err(e) => {
                            // A pool conflict is a configuration error, not a
                            // transient one: retrying every 10 s only floods the
                            // api and buries the cause in the journal. Exit so
                            // systemd shows the node as failed and the operator
                            // fixes the config instead of waiting for it to heal.
                            let text = e.to_string();
                            if text.contains("pool_inbounds_conflict") {
                                tracing::error!(
                                    "Registration refused by the api (pool_inbounds_conflict): {}",
                                    text
                                );
                                std::process::exit(1);
                            }
                            warn!("API unavailable, {} retrying... ", text);
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

fn adopt_private_env(config_path: &str, current: &impl serde::Serialize) -> bool {
    let Ok(wanted_raw) = std::fs::read_to_string("scope.env") else {
        return false;
    };
    let wanted = wanted_raw.trim();
    if wanted.is_empty() {
        return false;
    }
    let Ok(current_json) = serde_json::to_string(current) else {
        return false;
    };
    if wanted == current_json.trim_matches('"') {
        return false;
    }
    match rewrite_node_env(config_path, wanted) {
        Ok(()) => true,
        Err(e) => {
            error!("Failed to rewrite node env: {}", e);
            false
        }
    }
}

fn rewrite_node_env(path: &str, env_value: &str) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut out = String::new();
    let mut in_node = false;
    let mut replaced = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_node = trimmed == "[node]";
        }
        if in_node && !replaced && trimmed.starts_with("env") && trimmed.contains('=') {
            out.push_str(&format!("env = \"{env_value}\"\n"));
            replaced = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !replaced {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "node env key missing",
        ));
    }
    std::fs::write(path, out)
}

/// `|local - api| / api`, 0.0 when api reports nothing (avoid NaN in metrics).
fn divergence_ratio(local: u64, api: u64) -> f64 {
    if api == 0 {
        return 0.0;
    }
    (local.abs_diff(api) as f64) / (api as f64)
}
