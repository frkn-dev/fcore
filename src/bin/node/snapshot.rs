#[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
use rkyv::Archive;
#[cfg(feature = "xray")]
use std::sync::Arc;
#[cfg(feature = "xray")]
use tokio::sync::Mutex;

#[cfg(feature = "wireguard")]
use fcore::WgApi;

#[cfg(feature = "amnezia-wg")]
use fcore::AwgInterface;

#[cfg(feature = "xray")]
use fcore::{XrayHandlerActions, XrayHandlerClient};

#[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
use fcore::{Error, Result, Tag};

#[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
use fcore::{ConnectionBaseOperations, Connections, SnapshotManager};

#[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
#[async_trait::async_trait]
pub trait SnapshotRestore {
    #[cfg(feature = "wireguard")]
    async fn restore_wg_connections(&self, wg_client: Option<WgApi>) -> Result<()>;

    #[cfg(feature = "amnezia-wg")]
    async fn restore_awg_connections(&self, awg_client: Option<AwgInterface>) -> Result<()>;

    #[cfg(feature = "xray")]
    async fn restore_xray_connections(
        &self,
        xray_client: Option<Arc<Mutex<XrayHandlerClient>>>,
    ) -> Result<()>;
}

#[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
#[async_trait::async_trait]
impl<C> SnapshotRestore for SnapshotManager<Connections<C>>
where
    C: Archive + Send + Sync + Clone + 'static + ConnectionBaseOperations,
{
    #[cfg(feature = "wireguard")]
    async fn restore_wg_connections(&self, wg_client: Option<WgApi>) -> Result<()> {
        let mem = self.memory.read().await;

        if mem.is_empty() {
            return Err(Error::Custom("Empty snapshot".into()));
        }

        let conns: Vec<(uuid::Uuid, C)> = mem
            .iter()
            .filter(|(_, conn)| conn.get_proto().proto() == Tag::Wireguard)
            .map(|(id, conn)| (*id, conn.clone()))
            .collect();

        drop(mem);

        for (conn_id, conn) in conns {
            let wg_client = wg_client.clone();

            tokio::spawn(async move {
                if let Some(wg) = conn.get_wireguard() {
                    if let Some(api) = wg_client.as_ref() {
                        if let Ok(pubkey) = &wg.peer_pubkey() {
                            if let Err(e) = api.create(pubkey, wg.address.clone()) {
                                tracing::error!(
                                    "Failed to restore WireGuard connection {}: {}",
                                    conn_id,
                                    e
                                );
                            }
                        }
                    }
                }
            });
        }
        Ok(())
    }

    #[cfg(feature = "amnezia-wg")]
    async fn restore_awg_connections(&self, awg_client: Option<AwgInterface>) -> Result<()> {
        let mem = self.memory.read().await;

        if mem.is_empty() {
            return Err(Error::Custom("Empty snapshot".into()));
        }

        let conns: Vec<(uuid::Uuid, C)> = mem
            .iter()
            .filter(|(_, conn)| conn.get_proto().proto() == Tag::AmneziaWg)
            .map(|(id, conn)| (*id, conn.clone()))
            .collect();

        drop(mem);

        for (conn_id, conn) in conns {
            let awg_client = awg_client.clone();

            tokio::spawn(async move {
                if let Some(awg) = conn.get_amneziawg() {
                    if let Some(api) = awg_client.as_ref() {
                        let pubkey = match awg.keys.pubkey() {
                            Ok(pk) => pk,
                            Err(e) => {
                                tracing::error!(
                                    "Failed to derive AWG pubkey for connection {}: {}",
                                    conn_id,
                                    e
                                );
                                return;
                            }
                        };

                        let pubkey = match AwgInterface::decode_pubkey(&pubkey) {
                            Ok(pk) => pk,
                            Err(e) => {
                                tracing::error!(
                                    "Failed to decode AWG pubkey for connection {}: {}",
                                    conn_id,
                                    e
                                );
                                return;
                            }
                        };

                        let peer = netlink_packet_amnezia_wireguard::AmneziaWireguardPeer(vec![
                            netlink_packet_amnezia_wireguard::AmneziaWireguardPeerAttribute::PublicKey(pubkey),
                            netlink_packet_amnezia_wireguard::AmneziaWireguardPeerAttribute::AllowedIps(vec![awg.address.clone().into()]),
                        ]);

                        if let Err(e) = api.add_peer(peer) {
                            tracing::error!(
                                "Failed to restore AmneziaWG connection {}: {}",
                                conn_id,
                                e
                            );
                        }
                    }
                }
            });
        }
        Ok(())
    }

    #[cfg(feature = "xray")]
    async fn restore_xray_connections(
        &self,
        xray_client: Option<Arc<Mutex<XrayHandlerClient>>>,
    ) -> Result<()> {
        let mem = self.memory.read().await;

        if mem.is_empty() {
            return Err(Error::Custom("Empty snapshot".into()));
        }

        let conns: Vec<(uuid::Uuid, C)> = mem
            .iter()
            .filter(|(_, conn)| {
                let p = conn.get_proto().proto();
                matches!(
                    p,
                    Tag::VlessTcpReality
                        | Tag::VlessGrpcReality
                        | Tag::VlessXhttpReality
                        | Tag::VlessXhttpCdn
                        | Tag::Vmess
                        | Tag::Shadowsocks
                )
            })
            .map(|(id, conn)| (*id, conn.clone()))
            .collect();

        drop(mem);

        for (conn_id, conn) in conns {
            let xray_clone = xray_client.clone();
            tokio::spawn(async move {
                if let Some(client) = xray_clone {
                    let proto = conn.get_proto().proto();

                    let password = if proto == Tag::Shadowsocks {
                        conn.get_password()
                    } else {
                        None
                    };

                    if let Err(e) = client.create(&conn_id, proto, password).await {
                        tracing::error!("Failed to restore Xray {}: {}", conn_id, e);
                    } else {
                        tracing::debug!("Restored connection {}", conn_id);
                    }
                }
            });
        }

        Ok(())
    }
}
