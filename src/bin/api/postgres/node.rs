use chrono::DateTime;
use chrono::Utc;
use std::collections::HashMap;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::sync::Mutex;

use tracing::{debug, error, warn};

use fcore::{
    AmneziaWgSettings, AwgInterfaceConfig, AwgObfuscationParams, H2Settings, Inbound, IpAddrMask,
    Node, NodeStatus, NodeType, Result, WgKeys, WireguardSettings,
};

use super::pg::PgClientManager;

pub struct PgNode {
    pub manager: Arc<Mutex<PgClientManager>>,
}

impl PgNode {
    pub fn new(manager: Arc<Mutex<PgClientManager>>) -> Self {
        Self { manager }
    }

    pub async fn upsert(&self, node_id: uuid::Uuid, node: Node) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;
        let tx = client.transaction().await?;
        let cores = node.cores as i32;

        let address: IpAddr = IpAddr::V4(node.address);

        let node_query = "
        INSERT INTO nodes (
            id, uuid, env, hostname, address, status, created_at, modified_at, label, interface, cores, max_bandwidth_bps, country, node_type
        )
        VALUES (
            $1, $2, $3, $4, $5, $6::node_status, $7, $8, $9, $10, $11, $12, $13, $14::node_type
        )
        ON CONFLICT (id) DO UPDATE SET
            uuid = EXCLUDED.uuid,
            env = EXCLUDED.env,
            hostname = EXCLUDED.hostname,
            address = EXCLUDED.address,
            status = EXCLUDED.status,
            modified_at = EXCLUDED.modified_at,
            label = EXCLUDED.label,
            interface = EXCLUDED.interface,
            cores = EXCLUDED.cores,
            max_bandwidth_bps = EXCLUDED.max_bandwidth_bps,
            country = EXCLUDED.country,
            node_type = EXCLUDED.node_type
    ";

        tx.execute(
            node_query,
            &[
                &node_id,
                &node.uuid,
                &node.env.to_string(),
                &node.hostname,
                &address,
                &node.status,
                &node.created_at,
                &node.modified_at,
                &node.label,
                &node.interface,
                &cores,
                &node.max_bandwidth_bps,
                &node.country,
                &node.r#type,
            ],
        )
        .await?;

        let inbound_query = "
        INSERT INTO inbounds (
            id, node_id, tag, port, stream_settings,
            wg_privkey, wg_interface, wg_address, dns, h2, mtproto_secret,
            awg_privkey, awg_interface, awg_address, awg_dns, awg_mtu, awg_obfuscation
        )
        VALUES (
            $1, $2, $3, $4, $5,
            $6, $7, $8,
            $9, $10, $11,
            $12, $13, $14, $15, $16, $17
        )
        ON CONFLICT (node_id, tag) DO UPDATE SET
            port = EXCLUDED.port,
            stream_settings = EXCLUDED.stream_settings,
            wg_privkey = EXCLUDED.wg_privkey,
            wg_interface = EXCLUDED.wg_interface,
            wg_address = EXCLUDED.wg_address,
            dns = EXCLUDED.dns,
            h2 = EXCLUDED.h2,
            mtproto_secret = EXCLUDED.mtproto_secret,
            awg_privkey = EXCLUDED.awg_privkey,
            awg_interface = EXCLUDED.awg_interface,
            awg_address = EXCLUDED.awg_address,
            awg_dns = EXCLUDED.awg_dns,
            awg_mtu = EXCLUDED.awg_mtu,
            awg_obfuscation = EXCLUDED.awg_obfuscation
    ";

        for inbound in node.inbounds.values() {
            let inbound_id = uuid::Uuid::new_v4();
            let stream_settings = serde_json::to_value(&inbound.stream_settings)?;
            let h2_settings = serde_json::to_value(&inbound.h2)?;

            let (wg_privkey, wg_interface, wg_address, dns) = inbound
                .wg
                .as_ref()
                .map(|wg| {
                    (
                        Some(&wg.keys.privkey),
                        Some(&wg.interface),
                        Some(wg.address.to_string()),
                        Some(
                            wg.dns
                                .iter()
                                .cloned()
                                .map(IpAddr::V4)
                                .collect::<Vec<IpAddr>>(),
                        ),
                    )
                })
                .unwrap_or((None, None, None, None));

            let (awg_privkey, awg_interface, awg_address, awg_dns, awg_mtu, awg_obfuscation) =
                inbound
                    .awg
                    .as_ref()
                    .map(|awg| {
                        (
                            Some(&awg.interface.private_key.privkey),
                            Some(&awg.interface.interface),
                            Some(awg.interface.address.to_string()),
                            Some(
                                awg.interface
                                    .dns
                                    .iter()
                                    .cloned()
                                    .map(IpAddr::V4)
                                    .collect::<Vec<IpAddr>>(),
                            ),
                            awg.interface.mtu.map(|m| m as i16),
                            serde_json::to_value(&awg.obfuscation).ok(),
                        )
                    })
                    .unwrap_or((None, None, None, None, None, None));

            tx.execute(
                inbound_query,
                &[
                    &inbound_id,
                    &node_id,
                    &inbound.tag,
                    &(inbound.port as i32),
                    &stream_settings,
                    &wg_privkey,
                    &wg_interface,
                    &wg_address,
                    &dns,
                    &h2_settings,
                    &inbound.mtproto_secret,
                    &awg_privkey,
                    &awg_interface,
                    &awg_address,
                    &awg_dns,
                    &awg_mtu,
                    &awg_obfuscation,
                ],
            )
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn all(&self) -> Result<Vec<Node>> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let rows = client
            .query(
                "SELECT
                      n.id AS node_id,
                      n.uuid,
                      n.env,
                      n.hostname,
                      n.address,
                      n.status,
                      n.created_at,
                      n.modified_at,
                      n.label,
                      n.interface,
                      n.cores,
                      n.max_bandwidth_bps,
                      n.country,
                      n.node_type,

                      i.id AS inbound_id,
                      i.tag,
                      i.port,
                      i.stream_settings,

                      i.wg_privkey,
                      i.wg_interface,
                      i.wg_address,
                      i.dns,

                      i.awg_privkey,
                      i.awg_interface,
                      i.awg_address,
                      i.awg_dns,
                      i.awg_mtu,
                      i.awg_obfuscation,

                      i.h2,
                      i.mtproto_secret
                 FROM nodes n
                 LEFT JOIN inbounds i ON n.id = i.node_id",
                &[],
            )
            .await?;

        let mut nodes_map: HashMap<uuid::Uuid, Node> = HashMap::new();

        for row in rows {
            let node_id: uuid::Uuid = row.get("node_id");
            let uuid: uuid::Uuid = row.get("uuid");
            let env: String = row.get("env");
            let hostname: String = row.get("hostname");
            let address: IpAddr = row.get("address");
            let status: NodeStatus = row.get("status");
            let created_at: DateTime<Utc> = row.get("created_at");
            let modified_at: DateTime<Utc> = row.get("modified_at");
            let label: String = row.get("label");
            let interface: String = row.get("interface");
            let cores: i32 = row.get("cores");
            let country: String = row.get("country");
            let max_bandwidth_bps: i64 = row.get("max_bandwidth_bps");
            let r#type: NodeType = row.get("node_type");

            let wg_address: Option<IpAddrMask> = row
                .get::<_, Option<String>>("wg_address")
                .and_then(|s| s.parse().ok());

            let awg_address: Option<IpAddrMask> = row
                .get::<_, Option<String>>("awg_address")
                .and_then(|s| s.parse().ok());

            let awg_dns: Option<Vec<Ipv4Addr>> =
                row.get::<_, Option<Vec<IpAddr>>>("awg_dns").map(|ips| {
                    ips.into_iter()
                        .filter_map(|ip| match ip {
                            IpAddr::V4(v4) => Some(v4),
                            _ => None,
                        })
                        .collect()
                });

            let awg_obfuscation: Option<AwgObfuscationParams> = row
                .get::<_, Option<serde_json::Value>>("awg_obfuscation")
                .and_then(|v| serde_json::from_value(v).ok());

            let dns: Option<Vec<Ipv4Addr>> = row.get::<_, Option<Vec<IpAddr>>>("dns").map(|ips| {
                ips.into_iter()
                    .filter_map(|ip| match ip {
                        IpAddr::V4(v4) => Some(v4),
                        _ => None,
                    })
                    .collect()
            });
            let inbound_id: Option<uuid::Uuid> = row.get("inbound_id");

            let h2: Option<H2Settings> = row
                .get::<_, Option<serde_json::Value>>("h2")
                .and_then(|v| serde_json::from_value(v).ok());

            if let IpAddr::V4(ipv4) = address {
                let node_entry = nodes_map.entry(node_id).or_insert_with(|| Node {
                    uuid,
                    env: env.into(),
                    hostname: hostname.clone(),
                    address: ipv4,
                    interface: interface.clone(),
                    status,
                    created_at,
                    modified_at,
                    label: label.clone(),
                    inbounds: HashMap::new(),
                    cores: cores as usize,
                    max_bandwidth_bps,
                    country,
                    r#type,
                });

                if let Some(_inbound_id) = inbound_id {
                    let wg = match (
                        row.get::<_, Option<String>>("wg_privkey"),
                        row.get::<_, Option<String>>("wg_interface"),
                        wg_address,
                        dns,
                    ) {
                        (Some(privkey), Some(interface), Some(address), Some(dns)) => {
                            Some(WireguardSettings {
                                keys: WgKeys { privkey },
                                interface,
                                address,
                                port: row.get::<_, i32>("port") as u16,
                                dns,
                            })
                        }
                        _ => None,
                    };

                    let awg = match (
                        row.get::<_, Option<String>>("awg_privkey"),
                        row.get::<_, Option<String>>("awg_interface"),
                        awg_address,
                        awg_dns,
                    ) {
                        (Some(private_key), Some(interface), Some(address), Some(dns)) => {
                            Some(AmneziaWgSettings {
                                interface: AwgInterfaceConfig {
                                    interface,
                                    address,
                                    listen_port: row.get::<_, i32>("port") as u16,
                                    mtu: row
                                        .get::<_, Option<i16>>("awg_mtu")
                                        .map(|m| m as u16),
                                    private_key: WgKeys {
                                        privkey: private_key,
                                    },
                                    dns,
                                },
                                obfuscation: awg_obfuscation,
                            })
                        }
                        _ => None,
                    };

                    let mtproto_secret = row.get::<_, Option<String>>("mtproto_secret");

                    let inbound = Inbound {
                        tag: row.get("tag"),
                        port: row.get::<_, i32>("port") as u16,
                        stream_settings: row
                            .get::<_, Option<serde_json::Value>>("stream_settings")
                            .and_then(|v| serde_json::from_value(v).ok()),

                        wg,
                        awg,
                        h2,
                        mtproto_secret,
                    };

                    node_entry.inbounds.insert(inbound.tag, inbound);
                }
            }
        }

        Ok(nodes_map.into_values().collect())
    }

    pub async fn update_status(&self, uuid: &uuid::Uuid, new_status: NodeStatus) -> Result<()> {
        let mut manager = self.manager.lock().await;
        let client = manager.get_client().await?;

        let modified_at = Utc::now();
        let query = "
        UPDATE nodes
        SET status = $1::node_status, modified_at = $2
        WHERE uuid = $3
    ";

        let result = client
            .execute(query, &[&new_status, &modified_at, uuid])
            .await;

        match result {
            Ok(rows_updated) => {
                if rows_updated == 0 {
                    warn!("No node found with UUID {} ", uuid,);
                } else {
                    debug!("Updated node {} status to {} ", uuid, new_status);
                }
            }
            Err(e) => {
                error!("Error updating node status: {}", e);
                return Err(e.into());
            }
        }

        Ok(())
    }
}
