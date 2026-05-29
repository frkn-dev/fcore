use futures::Future;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tracing::{debug, error, info};

use super::{http::routes::Http, metrics::MetricWorker, service::Service, tasks::Tasks};
use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, NodeStorageOperations, Result,
    Subscriber, Subscription, SubscriptionOperations, Topic,
};

fn spawn_task<F>(name: &'static str, future: F) -> JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        info!("{name} started");

        future.await;

        error!("{name} unexpectedly stopped");
    })
}

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
    pub async fn run(self: Arc<Self>) -> Result<()> {
        spawn_task("metric_storage_gc", {
            let metrics_storage = Arc::clone(&self.metrics);

            async move {
                let mut interval = tokio::time::interval(Duration::from_secs(60));

                loop {
                    interval.tick().await;

                    let storage = Arc::clone(&metrics_storage);

                    tokio::task::spawn_blocking(move || {
                        debug!("Starting MetricStorage GC...");
                        storage.perform_gc();
                        debug!("MetricStorage GC finished.");
                    });
                }
            }
        });

        spawn_task("metric_storage_snapshot", {
            let metrics_storage = Arc::clone(&self.metrics);
            let snapshot_path = self.settings.metrics.snapshot_path.clone();

            async move {
                let mut interval = tokio::time::interval(Duration::from_secs(60));

                loop {
                    interval.tick().await;

                    debug!("Starting MetricStorage Snapshot");

                    if let Err(err) = metrics_storage.save_snapshot(&snapshot_path).await {
                        error!("Snapshot save failed: {}", err);
                    }

                    debug!("MetricStorage Snapshot finished");
                }
            }
        });

        spawn_task("monitor_node_heartbeats", {
            let service = Arc::clone(&self);

            let check_interval = self.settings.tasks.monitor_nodes_interval;

            let offline_threshold = self.settings.tasks.heartbeat_node_offline_threshold_sec;

            async move {
                service
                    .monitor_node_heartbeats(check_interval, offline_threshold)
                    .await;
            }
        });

        spawn_task("cleanup_expired_connections", {
            let interval = self.settings.tasks.connection_expire_interval;
            let service = Arc::clone(&self);
            async move {
                service.cleanup_expired_connections(interval).await;
            }
        });

        spawn_task("cleanup_expired_subscriptions", {
            let interval = self.settings.tasks.subscription_expire_interval;
            let service = Arc::clone(&self);
            async move {
                service.cleanup_expired_subscriptions(interval).await;
            }
        });

        spawn_task("restore_subscriptions", {
            let interval = self.settings.tasks.subscription_restore_interval;
            let service = Arc::clone(&self);
            async move {
                service.restore_subscriptions(interval).await;
            }
        });

        spawn_task("periodic_db_sync", {
            let interval = self.settings.tasks.db_sync_interval_sec;
            let service = Arc::clone(&self);

            async move {
                service.periodic_db_sync(interval).await;
            }
        });

        spawn_task("metric_worker", {
            let metrics = Arc::clone(&self.metrics);
            let receiver = self.settings.metrics.reciever.clone();

            async move {
                match Subscriber::new_bound(&receiver, vec![Topic::Metrics]) {
                    Ok(subscriber) => {
                        MetricWorker::start(metrics, subscriber).await;
                    }
                    Err(err) => {
                        error!("Failed to start MetricWorker: {}", err);
                    }
                }
            }
        });

        let service = Arc::clone(&self);
        let service_handle = tokio::spawn(async move {
            if let Err(e) = service.run_http().await {
                error!("API server exited with error: {}", e);
            }
        });

        tokio::select! {
                    _ = service_handle => {
                        println!("API server finished");
                        Ok(())
                    }
                    _ = tokio::signal::ctrl_c() => {
            info!("Saving metrics snapshot...");

            if let Err(err) =
                self.metrics
                    .save_snapshot(&self.settings.metrics.snapshot_path)
                    .await
            {
                error!("Failed to save snapshot: {}", err);
            }

            Ok(())
        }
                }
    }
}
