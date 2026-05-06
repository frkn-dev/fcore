pub mod config;
pub mod error;
pub mod http;
pub mod memory;
pub mod metrics;
pub mod proto;
pub mod utils;
pub mod zmq;

pub use error::{Error, Result, SyncError};

pub const BANNER: &str = r#"
//                       __      _
//                      / _|    | |
//                     | |_ _ __| | ___ __
//       _____      _  |  _| '__| |/ / '_ \
//      |  __ \    (_) | | | |  |   <| | | |
//      | |__) | __ ___|_|_|_| _|_|\_\_| |_|
//      |  ___/ '__| \ \ / / _` |/ __| | | |
//      | |   | |  | |\ V / (_| | (__| |_| |
//      |_|___|_|  |_| \_/ \__,_|\___|\__, |
//       / ____|                      __/  /
//      | |     ___  _ __ ___  _ __  | ___/_ __  _   _
//      | |    / _ \| '_ ` _ \| '_ \ / _` | '_ \| | | |
//      | |___| (_) | | | | | | |_) | (_| | | | | |_| |
//       \_____\___/|_| |_| |_| .__/ \__,_|_| |_|\__, |
//                            | |                 __/ /
//                            |_|                |___/
"#;

pub const VERSION: &str = "0.5.5";

pub use config::{
    clash::InboundClashConfig,
    h2::{H2Settings, Hysteria2Settings},
    inbound::{Inbound, InboundConnLink, Settings as XraySettings},
    mtproto::MtprotoSettings,
    settings::{ApiAccessConfig, MetricsTxConfig, NodeConfig, NodeConfigRaw, Settings},
    wireguard::{WireguardServerConfig, WireguardSettings},
};

pub use memory::{
    connection::{
        base::Base as BaseConnection,
        conn::Conn as Connection,
        operation::{
            api::Operations as ConnectionApiOperations,
            base::Operations as ConnectionBaseOperations,
        },
        proto::Proto,
        stat::Stat as ConnectionStat,
        wireguard::{IpAddrMask, Keys as WgKeys, Param as WgParam},
        Connections,
    },
    env::Env,
    key::{Code, Distributor, Key},
    node::{
        Node, NodeMetricInfo, NodeResponse, Stat as InboundStat, Status as NodeStatus,
        Type as NodeType,
    },
    snapshot::SnapshotManager,
    stat::{Kind as StatKind, Stat},
    storage::{
        connection::ApiOp as ConnectionStorageApiOperations,
        connection::BaseOp as ConnectionStorageBaseOperations,
        node::Operations as NodeStorageOperations,
        subscription::Operations as SubscriptionStorageOperations, Status,
    },
    subscription::{Operations as SubscriptionOperations, Subscription, Subscriptions},
    tag::ProtoTag as Tag,
};

pub use metrics::{
    storage::{HasMetrics, MetricBuffer, MetricStorage},
    MetricEnvelope, Metrics,
};
#[cfg(feature = "wireguard")]
pub use proto::wireguard::WgApi;
#[cfg(feature = "xray")]
pub use proto::xray::{
    client::{
        ConnOp as XrayConnOperation, HandlerActions as XrayHandlerActions,
        HandlerClient as XrayHandlerClient, StatsClient as XrayStatsClient, XrayClient,
    },
    stats::{Prefix, StatsOp},
};

pub use utils::*;

pub use zmq::{
    message::{Action, Message},
    publisher::Publisher,
    subscriber::Subscriber,
    topic::Topic,
};
