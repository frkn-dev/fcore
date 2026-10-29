use sha2::{Digest, Sha256};
use warp::http::StatusCode;

use fcore::{
    http::ResponseMessage, Connection, ConnectionApiOperations, ConnectionBaseOperations,
    NodeStorageOperations, Subscription, SubscriptionOperations,
};

use super::super::super::service::Cache;
use super::super::super::sync::MemSync;
use super::super::request::normalized_pool;

/// Cheap change detector for the server catalog. The hash moves when an
/// online node's server config revision changes, or when a node joins or
/// leaves the online set. Labels, countries and subscription rows do not.
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
        if node.status != fcore::NodeStatus::Online {
            continue;
        }
        hasher.update(id.as_bytes());
        let ips: Vec<String> = node
            .node_ips
            .as_ref()
            .map(|v| v.iter().map(|ip| ip.to_string()).collect())
            .unwrap_or_default();
        let mut tags: Vec<_> = node.inbounds.keys().copied().collect();
        tags.sort_by_key(|tag| tag.to_string());
        for tag in tags {
            if let Some(rev) = super::amnezia::config_rev(node, tag, &ips) {
                hasher.update(tag.to_string().as_bytes());
                hasher.update(rev.as_bytes());
            }
        }
        hasher.update(b"\n");
    }

    // Client feeds collapse a pool into a single row whose node_ips is the union
    // of every online member's entry addresses (see `pooled_entry_ips`). The loop
    // above only hashes each node's OWN addresses, so a pure membership change —
    // a node joins or leaves a pool, nothing else moves — left the version
    // unchanged: clients kept a stale single-address row and never tried the
    // pool's other entry address. Hash the pools as well.
    let mut pool_groups: Vec<(String, fcore::Env, String)> = Vec::new();
    for (_, node) in mem.nodes.iter_nodes() {
        if node.status != fcore::NodeStatus::Online {
            continue;
        }
        let Some(pool) = normalized_pool(node.pool.clone()) else {
            continue;
        };
        let env_label = node.env.to_string();
        if !pool_groups
            .iter()
            .any(|(e, _, p)| e == &env_label && p == &pool)
        {
            pool_groups.push((env_label, node.env.clone(), pool));
        }
    }
    pool_groups.sort_by(|a, b| (&a.0, &a.2).cmp(&(&b.0, &b.2)));
    for (env_label, env, pool) in &pool_groups {
        hasher.update(b"pool\x00");
        hasher.update(env_label.as_bytes());
        hasher.update(pool.as_bytes());
        // membership, not just the resulting addresses: the same address set
        // served by a different member set is a different feed row. Sorted, so
        // the hash stays stable across restarts (iter_nodes is a HashMap walk).
        let mut member_ids: Vec<uuid::Uuid> = mem
            .nodes
            .iter_nodes()
            .filter(|(_, node)| {
                node.status == fcore::NodeStatus::Online
                    && &node.env == env
                    && normalized_pool(node.pool.clone()).as_deref() == Some(pool.as_str())
            })
            .map(|(id, _)| *id)
            .collect();
        member_ids.sort();
        for id in &member_ids {
            hasher.update(id.as_bytes());
        }
        for ip in super::amnezia::pooled_entry_ips(&mem.nodes, env, pool) {
            hasher.update(ip.as_bytes());
        }
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
            pool: None,
            pool_translation: None,
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
            main_feed_excluded_conns: HashSet::new(),
        }
    }

    #[test]
    fn catalog_version_changes_on_node_status_flip() {
        let mut cache = test_cache();
        let node = test_node(
            Env::Ru,
            [(Tag::Wireguard, wg_inbound())].into_iter().collect(),
        );
        let node_id = node.uuid;
        cache.nodes.entry(Env::Ru).or_default().push(node);

        let (v1, n1) = catalog_version(&cache);
        assert_eq!(n1, 1);

        // same state, same hash
        let (v1_again, _) = catalog_version(&cache);
        assert_eq!(v1, v1_again);

        cache.nodes.get_mut(&Env::Ru).unwrap()[0].label = "Renamed".into();
        let (v_label, _) = catalog_version(&cache);
        assert_eq!(v1, v_label);

        cache.nodes.get_mut(&Env::Ru).unwrap()[0].address = "203.0.113.9".parse().unwrap();
        let (v_ip, _) = catalog_version(&cache);
        assert_ne!(v1, v_ip);
        cache.nodes.get_mut(&Env::Ru).unwrap()[0].address = "192.168.1.100".parse().unwrap();

        cache.nodes.get_mut(&Env::Ru).unwrap()[0].status = NodeStatus::Offline;
        let (v2, _) = catalog_version(&cache);
        assert_ne!(v1, v2);

        cache.nodes.get_mut(&Env::Ru).unwrap()[0].status = NodeStatus::Online;
        let (v3, _) = catalog_version(&cache);
        assert_eq!(v1, v3);
        cache.main_feed_nodes.insert(node_id);
        let (v4, _) = catalog_version(&cache);
        assert_eq!(v1, v4);
    }

    #[test]
    fn catalog_version_changes_on_pool_membership() {
        let mut cache = test_cache();

        let mut a = test_node(
            Env::Ru,
            [(Tag::Wireguard, wg_inbound())].into_iter().collect(),
        );
        let mut b = test_node(
            Env::Ru,
            [(Tag::Wireguard, wg_inbound())].into_iter().collect(),
        );
        a.address = "192.168.1.100".parse().unwrap();
        b.address = "192.168.1.101".parse().unwrap();
        a.pool = Some("RU Рилзопровод".to_string());
        b.pool = Some("RU Рилзопровод".to_string());
        cache.nodes.entry(Env::Ru).or_default().push(a);
        cache.nodes.entry(Env::Ru).or_default().push(b);

        let (pooled, _) = catalog_version(&cache);

        // Same online set, same addresses, same per-node configs — only the pool
        // membership changed. Without it the client keeps the stale single-address
        // row and has nothing to fail over to, so the version must move.
        cache.nodes.get_mut(&Env::Ru).unwrap()[1].pool = None;
        let (single, _) = catalog_version(&cache);
        assert_ne!(
            pooled, single,
            "pool membership must move the catalog version"
        );

        cache.nodes.get_mut(&Env::Ru).unwrap()[1].pool = Some("RU Рилзопровод".to_string());
        let (pooled_again, _) = catalog_version(&cache);
        assert_eq!(pooled, pooled_again);

        // A renamed pool produces different feed rows as well.
        cache.nodes.get_mut(&Env::Ru).unwrap()[1].pool = Some("RU-2".to_string());
        let (renamed, _) = catalog_version(&cache);
        assert_ne!(pooled, renamed);
    }
}
