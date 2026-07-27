use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::Client as PgClient;
use tokio_postgres::NoTls;

use tracing::{debug, error, trace, warn};

use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, ConnectionStorageApiOperations,
    Node, NodeStorageOperations, Result, Status, Subscription, SubscriptionOperations,
    SubscriptionStorageOperations,
};

use super::{
    super::{config::PostgresConfig, service::Cache},
    connection::{ConnRow, PgConn},
    iap::PgIap,
    keys::PgKey,
    node::PgNode,
    subscription::PgSubscription,
    traffic::PgTraffic,
};

pub struct PgClientManager {
    config: PostgresConfig,
    client: Option<PgClient>,
}

impl PgClientManager {
    pub async fn new(config: PostgresConfig) -> Result<Self> {
        Ok(Self {
            config,
            client: None,
        })
    }

    async fn connect(&mut self) -> Result<()> {
        let connection_line = format!(
            "host={} user={} dbname={} password={} port={}",
            self.config.host,
            self.config.username,
            self.config.db,
            self.config.password,
            self.config.port
        );

        let (client, connection) = tokio_postgres::connect(&connection_line, NoTls).await?;

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                error!("Postgres connection dropped: {}", e);
            }
        });

        self.client = Some(client);
        Ok(())
    }

    pub async fn get_client(&mut self) -> Result<&mut PgClient> {
        if self.client.is_none() {
            self.connect().await?;
        }

        // ping with simple query
        let client = self.client.as_mut().unwrap();
        if let Err(e) = client.simple_query("SELECT 1").await {
            warn!("PG ping failed: {}. Reconnecting...", e);
            self.connect().await?;
        }

        Ok(self.client.as_mut().unwrap())
    }
}

#[derive(Clone)]
pub struct PgContext {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgContext {
    pub async fn init(config: &PostgresConfig) -> Result<Self> {
        let manager = PgClientManager::new(config.clone()).await?;

        Ok(Self {
            manager: Arc::new(Mutex::new(manager)),
        })
    }

    pub fn node(&self) -> PgNode {
        PgNode::new(self.manager.clone())
    }

    pub fn conn(&self) -> PgConn {
        PgConn::new(self.manager.clone())
    }

    pub fn sub(&self) -> PgSubscription {
        PgSubscription::new(self.manager.clone())
    }

    pub fn traffic(&self) -> PgTraffic {
        PgTraffic::new(self.manager.clone())
    }

    pub fn key(&self) -> PgKey {
        PgKey::new(self.manager.clone())
    }

    pub fn iap(&self) -> PgIap {
        PgIap::new(self.manager.clone())
    }
}

#[async_trait::async_trait]
pub trait Tasks {
    async fn add_node(&mut self, db_node: Node) -> Result<()>;
    async fn add_conn(&mut self, db_conn: ConnRow) -> Result<Status>;
    async fn add_subscription(&mut self, db_sub: Subscription) -> Status;
}

#[async_trait::async_trait]
impl<N, C, S> Tasks for Cache<N, C, S>
where
    N: NodeStorageOperations + Send + Sync + Clone + 'static,
    C: ConnectionBaseOperations
        + ConnectionApiOperations
        + Send
        + Sync
        + Clone
        + 'static
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + 'static,
    C: std::convert::From<fcore::Connection>,
    N: std::default::Default,
    S: PartialEq,
    S: std::convert::From<fcore::Subscription>,
    S: std::default::Default,
{
    async fn add_conn(&mut self, db_conn: ConnRow) -> Result<Status> {
        let conn_id = db_conn.conn_id;
        let conn: Connection = db_conn.try_into()?;

        self.connections.add(&conn_id, conn.into()).map_err(|e| {
            format!(
                "Create: Failed to add connection {} to state: {}",
                conn_id, e
            )
            .into()
        })
    }
    async fn add_node(&mut self, db_node: Node) -> Result<()> {
        match self.nodes.add(db_node.clone()) {
            Ok(_) => {
                debug!("Node added to State: {}", db_node.uuid);
                Ok(())
            }
            Err(e) => Err(format!(
                "Create: Failed to add node {} to state: {}",
                db_node.uuid, e
            )
            .into()),
        }
    }

    async fn add_subscription(&mut self, db_sub: Subscription) -> Status {
        let id = db_sub.id;
        trace!("Processing subscription: {}", id);

        let status = self.subscriptions.add(db_sub.into());

        match &status {
            Status::Ok(_) => debug!("✓ Subscription {} stored", id),
            Status::Updated(_) => debug!("↻ Subscription {} updated", id),
            Status::AlreadyExist(_) => debug!("○ Subscription {} unchanged", id),
            _ => debug!("Not implemented {}", id),
        }

        status
    }
}
