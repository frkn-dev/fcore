use sha2::{Digest, Sha256};
use warp::http::StatusCode;

use fcore::{
    http::ResponseMessage, Connection, ConnectionApiOperations, ConnectionBaseOperations,
    NodeStorageOperations, Subscription, SubscriptionOperations,
};

use super::super::super::service::Cache;
use super::super::super::sync::MemSync;

/// Cheap change detector for the server catalog. The client polls this on a
/// timer instead of re-downloading the full subscription feed: when the hash
/// differs from the stored one, the feed changed (node went on/offline,
/// address/inbound/connection shape changed) and a reload is due.
pub fn catalog_version<N, C, S>(mem: &Cache<N, C, S>) -> (String, usize)
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let mut hasher = Sha256::new();

    let mut nodes: Vec<_> = mem.nodes.iter_nodes().collect();
    nodes.sort_by_key(|(id, _)| *id);
    for (id, node) in &nodes {
        hasher.update(id.as_bytes());
        hasher.update(
            format!(
                "{:?}|{}|{}|{:?}|{}|{:?}|{:?}|",
                node.env, node.hostname, node.address, node.status, node.label, node.cluster,
                node.node_ips
            )
            .as_bytes(),
        );
        let mut inbounds: Vec<_> = node.inbounds.values().collect();
        inbounds.sort_by_key(|inbound| (inbound.tag.to_string(), inbound.port));
        for inbound in inbounds {
            hasher.update(format!("{}|{};", inbound.tag, inbound.port).as_bytes());
        }
        hasher.update(b"\n");
    }

    let mut feed: Vec<_> = mem.main_feed_nodes.iter().collect();
    feed.sort();
    for id in feed {
        hasher.update(id.as_bytes());
    }
    hasher.update(b"\n");

    let mut conns: Vec<_> = mem.connections.iter().collect();
    conns.sort_by_key(|(id, _)| *id);
    for (id, conn) in &conns {
        let conn: Connection = (*conn).clone().into();
        hasher.update(id.as_bytes());
        hasher.update(
            format!(
                "{}|{:?}|{}|{:?}|{:?};",
                conn.get_proto().proto(),
                conn.get_env(),
                conn.get_deleted(),
                mem.conn_labels.get(*id),
                mem.conn_nodes.get(*id)
            )
            .as_bytes(),
        );
    }

    (format!("{:x}", hasher.finalize()), nodes.len())
}

pub async fn get_catalog_version_handler<N, C, S>(
    memory: MemSync<N, C, S>,
) -> Result<impl warp::Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let mem = memory.memory.read().await;
    let (version, nodes) = catalog_version(&mem);

    let response = ResponseMessage {
        status: StatusCode::OK.as_u16(),
        message: "Catalog version".to_string(),
        response: Some(serde_json::json!({
            "version": version,
            "nodes": nodes,
        })),
    };

    Ok(warp::reply::with_status(
        warp::reply::json(&response),
        StatusCode::OK,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcore::{Env, Inbound, Node, NodeStatus, NodeType, Tag};
    use std::collections::{HashMap, HashSet};

    fn test_node(env: Env, inbounds: HashMap<Tag, Inbound>) -> Node {
        Node {
            uuid: uuid::Uuid::new_v4(),
            env,
            hostname: "test-node".to_string(),
            address: "192.168.1.100".parse().unwrap(),
            status: NodeStatus::Online,
            label: "Test".to_string(),
            interface: "eth0".to_string(),
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
            inbounds,
            cores: 4,
            max_bandwidth_bps: 1_000_000_000,
            country: "RU".to_string(),
            r#type: NodeType::Node,
            cluster: None,
            node_ips: None,
        }
    }

    fn wg_inbound() -> Inbound {
        Inbound {
            tag: Tag::Wireguard,
            port: 51820,
            stream_settings: None,
            wg: None,
            awg: None,
            h2: None,
            mtproto_secret: None,
        }
    }

    type TestCache = Cache<HashMap<Env, Vec<Node>>, Connection, Subscription>;

    fn test_cache() -> TestCache {
        Cache {
            nodes: HashMap::new(),
            connections: Default::default(),
            subscriptions: Default::default(),
            conn_labels: HashMap::new(),
            share_conns: HashSet::new(),
            conn_nodes: HashMap::new(),
            main_feed_nodes: HashSet::new(),
        }
    }

    #[test]
    fn catalog_version_changes_on_node_status_flip() {
        let mut cache = test_cache();
        let node = test_node(Env::Ru, [(Tag::Wireguard, wg_inbound())].into_iter().collect());
        let node_id = node.uuid;
        cache.nodes.entry(Env::Ru).or_default().push(node);

        let (v1, n1) = catalog_version(&cache);
        assert_eq!(n1, 1);

        // same state, same hash
        let (v1_again, _) = catalog_version(&cache);
        assert_eq!(v1, v1_again);

        // status flip changes the hash
        cache.nodes.get_mut(&Env::Ru).unwrap()[0].status = NodeStatus::Offline;
        let (v2, _) = catalog_version(&cache);
        assert_ne!(v1, v2);

        // feed membership changes the hash too
        cache.nodes.get_mut(&Env::Ru).unwrap()[0].status = NodeStatus::Online;
        let (v3, _) = catalog_version(&cache);
        assert_eq!(v1, v3);
        cache.main_feed_nodes.insert(node_id);
        let (v4, _) = catalog_version(&cache);
        assert_ne!(v1, v4);
    }
}
