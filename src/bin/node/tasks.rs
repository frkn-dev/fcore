use async_trait::async_trait;
use futures::future::try_join_all;
use rkyv::{AlignedVec, Deserialize, Infallible};
use tokio::time::Duration;
#[cfg(feature = "xray")]
use tonic::Status;

use fcore::{Action, BaseConnection as Connection, Message, Metrics, Topic};
#[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
use fcore::{ConnectionStorageBaseOperations, Proto, Tag};
use fcore::{Error, Result};
#[cfg(feature = "xray")]
use fcore::{StatsOp, XrayHandlerActions};

use fcore::ConnectionBaseOperations;

#[cfg(any(feature = "xray", feature = "wireguard"))]
use super::metrics::BusinessMetrics;
use super::node::Node;

#[cfg(feature = "amnezia-wg")]
use netlink_packet_amnezia_wireguard::{
    WireguardAddressFamily, WireguardAllowedIpAttr, WireguardPeer, WireguardPeerAttribute,
};

#[cfg(feature = "amnezia-wg")]
use fcore::AwgInterface;

#[async_trait]
pub trait Tasks {
    async fn run_subscriber(&self) -> Result<()>;
    async fn handle_messages_batch(&self, msg: Vec<Message>) -> Result<()>;
    async fn handle_message(&self, msg: Message) -> Result<()>;
    async fn collect_metrics(&self);
}

#[async_trait]
impl<C> Tasks for Node<C>
where
    C: ConnectionBaseOperations + Send + Sync + Clone + 'static + From<Connection>,
{
    async fn run_subscriber(&self) -> Result<()> {
        let sub = self.subscriber.clone();
        let node_uuid = self.node.uuid;
        let node_env = &self.node.env;

        loop {
            let Some((topic_bytes, payload_bytes)) = sub.recv().await else {
                tracing::warn!("SUB: No multipart message received");
                continue;
            };

            let topic_str = std::str::from_utf8(&topic_bytes)
                .map_err(|_| Error::Custom("Invalid UTF8 topic".into()))?;

            let topic = match topic_str.parse::<Topic>() {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("SUB: Failed to parse topic '{}': {}", topic_str, e);
                    continue;
                }
            };

            tracing::debug!("SUB: Received topic: {:?}", topic);

            match &topic {
                Topic::Auth | Topic::Metrics => {
                    tracing::trace!("SUB: Ignoring unhandled topic: {:?}", topic);
                    continue;
                }

                Topic::Init(uuid) if uuid != &node_uuid => {
                    tracing::trace!("SUB: Skipping init for another node: {}", uuid);
                    continue;
                }

                Topic::Updates(env) if env != node_env => {
                    tracing::trace!("SUB: Skipping update for another env: {}", env);
                    continue;
                }

                _ => {
                    tracing::debug!("SUB: Accepted for processing: {:?}", topic);
                }
            }

            if payload_bytes.is_empty() {
                continue;
            }

            let messages: Option<Vec<Message>> = {
                let mut aligned = AlignedVec::new();
                aligned.extend_from_slice(&payload_bytes);

                match rkyv::check_archived_root::<Vec<Message>>(&aligned) {
                    Ok(archived) => archived.deserialize(&mut Infallible).ok(),
                    Err(e) => {
                        tracing::error!("SUB: Invalid rkyv root: {:?}", e);
                        None
                    }
                }
            };

            if let Some(msgs) = messages {
                if let Err(err) = self.handle_messages_batch(msgs).await {
                    tracing::error!("SUB: Failed to handle messages: {}", err);
                }
            }

            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    async fn handle_messages_batch(&self, messages: Vec<Message>) -> Result<()> {
        tracing::debug!("Got {} messages", messages.len());

        let handles: Vec<_> = messages
            .into_iter()
            .map(|msg| self.handle_message(msg))
            .collect();

        let _results = try_join_all(handles).await?;

        Ok(())
    }

    async fn handle_message(&self, msg: Message) -> Result<()> {
        match msg.action {
            Action::Create | Action::Update => {
                #[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
                let conn_id: uuid::Uuid = msg.conn_id;

                match msg.tag {
                    #[cfg(feature = "amnezia-wg")]
                    Tag::AmneziaWg => {
                        let awg = msg.wg.clone().ok_or_else(|| {
                            Error::Custom("Missing Amnezia WireGuard keys".into())
                        })?;

                        let proto = Proto::new_awg(&awg);

                        let conn = Connection::new(
                            proto,
                            msg.expires_at.map(Into::into),
                            msg.subscription_id,
                        );

                        {
                            let mut mem = self.memory.write().await;
                            mem.add(&conn_id, conn.clone().into()).map_err(|err| {
                                Error::Custom(format!(
                                    "Failed to add Amnezia WireGuard conn {}: {}",
                                    conn_id, err
                                ))
                            })?;
                        }

                        let awg_api = self.awg_client.as_ref().ok_or_else(|| {
                            Error::Custom("Amnezia WireGuard API is unavailable".into())
                        })?;

                        // pubkey
                        let pubkey = awg
                            .keys
                            .pubkey()
                            .map_err(|e| Error::Custom(format!("Invalid AWG pubkey: {}", e)))?;

                        let pubkey = AwgInterface::decode_pubkey(&pubkey)?;

                        // optional: check existence via netlink
                        let device = awg_api.get_device().map_err(|e| {
                            Error::Custom(format!("Failed to read AWG device: {}", e))
                        })?;

                        let exists = device.peers.iter().any(|p| {
                            p.0.iter().any(|attr| {
            matches!(attr, WireguardPeerAttribute::PublicKey(k) if k == &pubkey)
        })
                        });

                        if exists {
                            return Err(Error::Custom("AWG user already exists".into()));
                        }

                        let peer = WireguardPeer(vec![
                            WireguardPeerAttribute::PublicKey(pubkey),
                            WireguardPeerAttribute::AllowedIps(vec![awg.address.into()]),
                        ]);

                        tracing::error!("{:#?}", peer);
                        awg_api.add_peer(peer).map_err(|e| {
                            Error::Custom(format!(
                                "Failed to create AmneziaWG peer via netlink: {}",
                                e
                            ))
                        })?;

                        tracing::debug!("AWG connection created {}", conn);

                        return Ok(());
                    }
                    #[cfg(feature = "wireguard")]
                    Tag::Wireguard => {
                        let wg = msg
                            .wg
                            .clone()
                            .ok_or_else(|| Error::Custom("Missing WireGuard keys".into()))?;

                        let proto = Proto::new_wg(&wg);

                        let conn = Connection::new(
                            proto,
                            msg.expires_at.map(Into::into),
                            msg.subscription_id,
                        );

                        {
                            let mut mem = self.memory.write().await;
                            mem.add(&conn_id.clone(), conn.clone().into())
                                .map_err(|err| {
                                    Error::Custom(format!(
                                        "Failed to add WireGuard conn {}: {}",
                                        conn_id, err
                                    ))
                                })?;
                        }

                        let wg_api = self
                            .wg_client
                            .as_ref()
                            .ok_or_else(|| Error::Custom("WireGuard API is unavailable".into()))?;

                        if let Ok(pubkey) = wg.keys.pubkey() {
                            let exist = wg_api.is_exist(pubkey.clone());

                            if exist {
                                return Err(Error::Custom("WG User already exist".into()));
                            }

                            wg_api.create(&pubkey, wg.address).map_err(|e| {
                                Error::Custom(format!("Failed to create WireGuard peer: {}", e))
                            })?;

                            tracing::debug!("Connection Created {}", conn);
                        }
                        return Ok(());
                    }
                    #[cfg(feature = "xray")]
                    Tag::VlessTcpReality
                    | Tag::VlessGrpcReality
                    | Tag::VlessXhttpReality
                    | Tag::Vmess => {
                        let proto = Proto::new_xray(&msg.tag);
                        let conn = Connection::new(
                            proto,
                            msg.expires_at.map(Into::into),
                            msg.subscription_id,
                        );

                        let client = self.handler_client.as_ref().ok_or_else(|| {
                            Error::Grpc(Box::new(Status::unavailable("Xray handler unavailable")))
                        })?;

                        client
                            .create(&conn_id.clone(), msg.tag, None)
                            .await
                            .map_err(|err| {
                                Error::Custom(format!(
                                    "Failed to create conn {}: {}",
                                    conn_id.clone(),
                                    err
                                ))
                            })?;

                        let mut mem = self.memory.write().await;
                        mem.add(&conn_id.clone(), conn.into()).map_err(|err| {
                            Error::Custom(format!("Failed to add conn {}: {}", conn_id, err))
                        })?;

                        return Ok(());
                    }
                    #[cfg(feature = "xray")]
                    Tag::Shadowsocks => {
                        if let Some(password) = msg.password {
                            let proto = Proto::new_ss(&password);
                            let conn = Connection::new(
                                proto,
                                msg.expires_at.map(Into::into),
                                msg.subscription_id,
                            );

                            let client = self.handler_client.as_ref().ok_or_else(|| {
                                Error::Grpc(Box::new(Status::unavailable(
                                    "Xray handler unavailable",
                                )))
                            })?;

                            client
                                .create(&conn_id.clone(), msg.tag, Some(password))
                                .await
                                .map_err(|err| {
                                    Error::Custom(format!(
                                        "Failed to create conn {}: {}",
                                        conn_id, err
                                    ))
                                })?;

                            let mut mem = self.memory.write().await;
                            mem.add(&conn_id.clone(), conn.into()).map_err(|err| {
                                Error::Custom(format!("Failed to add conn {}: {}", conn_id, err))
                            })?;

                            return Ok(());
                        } else {
                            return Err(Error::Custom(
                                "Password not provided for Shadowsocks user".into(),
                            ));
                        }
                    }
                    _ => {
                        return Err(Error::Custom(
                            "Is not supported, or built without support the proto".into(),
                        ))
                    }
                }
            }
            Action::Delete => {
                let tag = msg.tag;
                #[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
                let conn_id = msg.conn_id;
                match tag {
                    #[cfg(feature = "amnezia-wg")]
                    Tag::AmneziaWg => {
                        let awg_api = self
                            .awg_client
                            .as_ref()
                            .ok_or_else(|| Error::Custom("AWG API is unavailable".into()))?;

                        let wg = msg
                            .wg
                            .clone()
                            .ok_or_else(|| Error::Custom("Missing AWG keys in message".into()))?;

                        let pubkey = wg.keys.pubkey()?;
                        let pubkey = AwgInterface::decode_pubkey(&pubkey)?;
                        awg_api.remove_peer(pubkey).map_err(|e| {
                            Error::Custom(format!("Failed to remove AWG peer: {}", e))
                        })?;

                        let mut mem = self.memory.write().await;
                        let _ = mem.remove(&conn_id);

                        return Ok(());
                    }
                    #[cfg(feature = "wireguard")]
                    Tag::Wireguard => {
                        let wg_api = self
                            .wg_client
                            .as_ref()
                            .ok_or_else(|| Error::Custom("WireGuard API is unavailable".into()))?;

                        let wg = msg.wg.clone().ok_or_else(|| {
                            Error::Custom("Missing WireGuard keys in message".into())
                        })?;

                        wg_api.delete(&wg.keys.pubkey()?).map_err(|e| {
                            Error::Custom(format!("Failed to delete WireGuard peer: {}", e))
                        })?;

                        let mut mem = self.memory.write().await;
                        let _ = mem.remove(&conn_id);

                        return Ok(());
                    }
                    #[cfg(feature = "xray")]
                    Tag::VlessTcpReality
                    | Tag::VlessGrpcReality
                    | Tag::VlessXhttpReality
                    | Tag::Vmess
                    | Tag::Shadowsocks => {
                        let client = self.handler_client.as_ref().ok_or_else(|| {
                            Error::Grpc(Box::new(Status::unavailable("Xray handler unavailable")))
                        })?;

                        client
                            .remove(&conn_id.clone(), tag, msg.password)
                            .await
                            .map_err(|e| {
                                Error::Custom(format!(
                                    "Couldn't remove connection from Xray: {}",
                                    e
                                ))
                            })?;

                        let mut mem = self.memory.write().await;
                        let _ = mem.remove(&conn_id);

                        return Ok(());
                    }

                    _ => {
                        return Err(Error::Custom(
                            "Is not supported, or built without support the proto".into(),
                        ))
                    }
                }
            }

            Action::ResetStat => {
                #[cfg(feature = "xray")]
                self.reset(&msg.conn_id)
                    .await
                    .map_err(|e| Error::Custom(format!("Couldn't reset stat: {}", e)))?;
                tracing::debug!("Reset stat for {}", msg.conn_id);
                return Ok(());
            }
        }
    }

    async fn collect_metrics(&self) {
        self.heartbeat().await;
        self.bandwidth().await;
        self.cpu_usage().await;
        self.loadavg().await;
        self.memory().await;
        self.disk_usage().await;
        #[cfg(feature = "xray")]
        if self.stats_client.is_some() {
            self.collect_inbound_metrics().await;
            self.collect_user_metrics().await;
        }
        #[cfg(feature = "wireguard")]
        if self.wg_client.is_some() {
            self.collect_wg_metrics().await;
        }
    }
}
