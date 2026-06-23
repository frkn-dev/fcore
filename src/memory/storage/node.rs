use serde_json::json;
use std::collections::HashMap;

use super::super::env::Env;
use super::super::node::Node;
use super::super::storage::Status as OperationStatus;
use crate::error::Result;

pub trait Operations {
    fn clear(&mut self) -> Result<()>;
    fn iter_nodes(&self) -> Box<dyn Iterator<Item = (&uuid::Uuid, &Node)> + '_>;
    fn add(&mut self, new_node: Node) -> Result<OperationStatus>;
    fn all(&self) -> Option<Vec<Node>>;
    fn all_json(&self) -> serde_json::Value;
    fn get_by_env(&self, env: &Env) -> Option<Vec<Node>>;
    fn get_mut_by_env(&mut self, env: &Env) -> Option<&mut Vec<Node>>;
    fn get_by_id(&self, id: &uuid::Uuid) -> Option<Node>;
    fn get_by_cluster(&self, cluster: &str) -> Vec<Node>;
    fn all_clusters(&self) -> Vec<String>;
    fn get(&self, env: &Env, uuid: &uuid::Uuid) -> Option<&Node>;
    fn get_mut(&mut self, env: &Env, uuid: &uuid::Uuid) -> Option<&mut Node>;
}

impl Operations for HashMap<Env, Vec<Node>> {
    fn clear(&mut self) -> Result<()> {
        self.clear();
        Ok(())
    }
    fn iter_nodes(&self) -> Box<dyn Iterator<Item = (&uuid::Uuid, &Node)> + '_> {
        let all_nodes: Vec<(&uuid::Uuid, &Node)> = self
            .values()
            .flat_map(|nodes_vec| nodes_vec.iter())
            .map(|node| (&node.uuid, node))
            .collect();

        Box::new(all_nodes.into_iter())
    }
    fn add(&mut self, new_node: Node) -> Result<OperationStatus> {
        let env = new_node.env.clone();
        let uuid = new_node.uuid;

        match self.get_mut_by_env(&env) {
            Some(nodes) => {
                for node in nodes.iter_mut() {
                    if node.uuid == uuid {
                        if node == &new_node {
                            return Ok(OperationStatus::AlreadyExist(uuid));
                        } else {
                            *node = new_node;
                            return Ok(OperationStatus::Updated(uuid));
                        }
                    }
                }
                nodes.push(new_node);
            }
            None => {
                self.insert(env, vec![new_node]);
            }
        }

        Ok(OperationStatus::Ok(uuid))
    }
    fn get(&self, env: &Env, uuid: &uuid::Uuid) -> Option<&Node> {
        self.get(env)?.iter().find(|n| &n.uuid == uuid)
    }
    fn get_mut(&mut self, env: &Env, uuid: &uuid::Uuid) -> Option<&mut Node> {
        self.get_mut(env)?.iter_mut().find(|n| &n.uuid == uuid)
    }
    fn get_by_env(&self, env: &Env) -> Option<Vec<Node>> {
        self.get(env).cloned()
    }
    fn get_mut_by_env(&mut self, env: &Env) -> Option<&mut Vec<Node>> {
        self.get_mut(env)
    }
    fn get_by_id(&self, node_id: &uuid::Uuid) -> Option<Node> {
        self.values()
            .flat_map(|nodes| nodes.iter())
            .find(|node| &node.uuid == node_id)
            .cloned()
    }
    fn get_by_cluster(&self, cluster: &str) -> Vec<Node> {
        self.values()
            .flat_map(|nodes| nodes.iter())
            .filter(|node| node.cluster.as_deref() == Some(cluster))
            .cloned()
            .collect()
    }
    fn all_clusters(&self) -> Vec<String> {
        let mut clusters: Vec<String> = self
            .values()
            .flat_map(|nodes| nodes.iter())
            .filter_map(|node| node.cluster.clone())
            .collect();
        clusters.sort();
        clusters.dedup();
        clusters
    }
    fn all(&self) -> Option<Vec<Node>> {
        let nodes: Vec<Node> = self.values().flatten().cloned().collect();

        (!nodes.is_empty()).then_some(nodes)
    }
    fn all_json(&self) -> serde_json::Value {
        let nodes: Vec<&Node> = self.values().flat_map(|v| v.iter()).collect();
        serde_json::to_value(&nodes).unwrap_or_else(|_| json!([]))
    }
}
