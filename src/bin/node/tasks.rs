use async_trait::async_trait;
use futures::future::try_join_all;
use rkyv::{AlignedVec, Deserialize, Infallible};
use tokio::time::Duration;
#[cfg(feature = "xray")]
use tonic::Status;

use fcore::{
    Action, BaseConnection as Connection, Message, Metrics, PeerIdentity, PeerMessage, Topic,
};
#[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
use fcore::{ConnectionStorageBaseOperations, Proto};
use fcore::{Error, Result, Tag};
#[cfg(feature = "xray")]
use fcore::{StatsOp, XrayHandlerActions};

use fcore::ConnectionBaseOperations;

#[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
use super::metrics::BusinessMetrics;
use super::node::Node;
use super::peer_plane;

#[cfg(feature = "amnezia-wg")]
use netlink_packet_amnezia_wireguard::{AmneziaWireguardPeer, AmneziaWireguardPeerAttribute};

#[cfg(feature = "amnezia-wg")]
use fcore::AwgInterface;

#[async_trait]
pub trait Tasks {
    async fn run_subscriber(&self) -> Result<()>;
    async fn handle_messages_batch(&self, msg: Vec<Message>) -> Result<()>;
    async fn handle_message(&self, msg: Message) -> Result<()>;
    async fn collect_metrics(&self);
    /// Apply a batch of peer changes. `authoritative` is set for
    /// `peers-init-<uuid>`: after applying, peers the api did not list are
    /// removed (that is how a revoked tunnel dies on a node that was down).
    async fn handle_peer_batch(&self, msgs: Vec<PeerMessage>, authoritative: bool) -> Result<()>;
    /// Drop connections of `tag` that the api did not list in its full sync.
    async fn prune_tag(
        &self,
        tag: Tag,
        keep: &std::collections::HashSet<uuid::Uuid>,
    ) -> Result<usize>;
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
        // Compare the whole topic string: `peers-update-<env>-<pool>` cannot be
        // split reliably (both halves may contain '-'), so the node only
        // accepts the exact topic it subscribed to.
        let own_peers_topic =
            peer_plane::peers_topic(self.node.env.clone(), self.node.pool.as_deref());

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

                Topic::PeersUpdates(..) if topic_str != own_peers_topic => {
                    tracing::trace!(
                        "SUB: Skipping peers update for another env/pool: {}",
                        topic_str
                    );
                    continue;
                }

                Topic::PeersInit(uuid) if uuid != &node_uuid => {
                    tracing::trace!("SUB: Skipping peers init for another node: {}", uuid);
                    continue;
                }

                _ => {
                    tracing::debug!("SUB: Accepted for processing: {:?}", topic);
                }
            }

            if payload_bytes.is_empty() {
                continue;
            }

            let is_peers = matches!(topic, Topic::PeersUpdates(..) | Topic::PeersInit(_));
            if is_peers {
                let peers: Option<Vec<PeerMessage>> = {
                    let mut aligned = AlignedVec::new();
                    aligned.extend_from_slice(&payload_bytes);
                    match rkyv::check_archived_root::<Vec<PeerMessage>>(&aligned) {
                        Ok(archived) => archived.deserialize(&mut Infallible).ok(),
                        Err(e) => {
                            tracing::error!("SUB: Invalid peers payload: {:?}", e);
                            None
                        }
                    }
                };
                if let Some(msgs) = peers {
                    let authoritative = matches!(topic, Topic::PeersInit(_));
                    if let Err(err) = self.handle_peer_batch(msgs, authoritative).await {
                        tracing::error!("SUB: Failed to apply peers: {}", err);
                    }
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
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

        // A full sync starts from a snapshot whose peers are already on the
        // interfaces. Re-sending Create for those ids fails with "already
        // exists" and aborts the whole batch, so skip the ones already in
        // memory while still counting them as seen.
        let full_tags = self.full_sync.lock().await.pending_tags();
        let existing: std::collections::HashSet<(Tag, uuid::Uuid)> = {
            let mem = self.memory.read().await;
            mem.iter()
                .map(|(id, conn)| (conn.get_proto().proto(), *id))
                .collect()
        };

        let mut to_apply = Vec::with_capacity(messages.len());
        let mut tracked: Vec<(Tag, uuid::Uuid)> = Vec::new();
        for msg in messages {
            if matches!(msg.action, Action::Create | Action::Update) {
                let full = full_tags.contains(&msg.tag);
                if matches!(msg.action, Action::Create)
                    && full
                    && existing.contains(&(msg.tag, msg.conn_id))
                {
                    tracing::debug!("full sync: keeping existing {} {}", msg.tag, msg.conn_id);
                    tracked.push((msg.tag, msg.conn_id));
                    continue;
                }
                tracked.push((msg.tag, msg.conn_id));
            }
            to_apply.push(msg);
        }

        let handles: Vec<_> = to_apply
            .into_iter()
            .map(|msg| self.handle_message(msg))
            .collect();

        if let Err(err) = try_join_all(handles).await {
            // A partial application must not leave a full sync armed: a later
            // delta would then be mistaken for the authoritative list.
            let mut pending = self.full_sync.lock().await;
            for (tag, _) in &tracked {
                pending.cancel(*tag);
            }
            return Err(err);
        }

        {
            let mut pending = self.full_sync.lock().await;
            for (tag, conn_id) in tracked {
                pending.record(tag, conn_id);
            }
        }

        // The expected count is supplied by sync_connections(); this releases a
        // full sync only when every promised id arrived and was applied.
        let to_prune = self.full_sync.lock().await.take_ready();
        for (tag, keep) in to_prune {
            match self.prune_tag(tag, &keep).await {
                Ok(0) => {}
                Ok(removed) => tracing::warn!(
                    "prune: tag={:?} removed={} (api listed {})",
                    tag,
                    removed,
                    keep.len()
                ),
                Err(e) => tracing::warn!("prune: tag={:?} failed: {}", tag, e),
            }
        }

        // Write-through: state changed, so the file on disk must change too.
        self.snapshot_wake.notify_one();

        Ok(())
    }

    async fn handle_peer_batch(&self, msgs: Vec<PeerMessage>, authoritative: bool) -> Result<()> {
        // Every pair the api listed: the prune keep-set is keyed by
        // `(public_key, address)`, never by the key alone, or one pool's peer
        // would vouch for the address of another.
        let mut keep: std::collections::HashMap<Tag, std::collections::HashSet<PeerIdentity>> =
            std::collections::HashMap::new();

        for msg in &msgs {
            let identity = PeerIdentity::new(&msg.public_key, peer_plane::host(&msg.address));
            // Listed is listed, however the apply turns out: the mirror is the
            // api's desired state, the interface is reconciled towards it.
            keep.entry(msg.tag).or_default().insert(identity.clone());
            match msg.action {
                Action::Create | Action::Update => {
                    // What this key must allow on the interface: the other
                    // pools' addresses plus this one. A row that moved to a new
                    // address loses its old entry on the way.
                    let desired = {
                        let mirror = self.peers.read().await;
                        let mut wanted = peer_plane::desired_except(
                            &mirror,
                            msg.tag,
                            &msg.public_key,
                            msg.peer_id,
                        );
                        wanted.insert(peer_plane::host(&msg.address).to_string());
                        wanted
                    };
                    let mut mirror = self.peers.write().await;
                    peer_plane::insert(
                        &mut mirror,
                        fcore::SnapshotPeer {
                            peer_id: msg.peer_id,
                            subscription_id: msg.subscription_id,
                            tag: msg.tag,
                            public_key: msg.public_key.clone(),
                            address: msg.address.clone(),
                        },
                    );
                    drop(mirror);
                    // One broken peer must not abort the batch: the rest are
                    // valid, and the failed one is retried by the next
                    // authoritative reconcile.
                    if let Err(e) = self.reconcile_peer(msg.tag, &msg.public_key, &desired) {
                        tracing::error!(
                            "peers: apply {} {} failed: {}",
                            msg.public_key,
                            msg.address,
                            e
                        );
                    }
                }
                Action::Delete => {
                    // Drop exactly this pair. The key keeps its other pools'
                    // addresses, and the kernel peer goes only when none is left.
                    let desired = {
                        let mirror = self.peers.read().await;
                        let mut wanted = peer_plane::desired(&mirror, msg.tag, &msg.public_key);
                        wanted.remove(peer_plane::host(&msg.address));
                        wanted
                    };
                    if let Err(e) = self.reconcile_peer(msg.tag, &msg.public_key, &desired) {
                        tracing::error!(
                            "peers: drop {} {} failed: {}",
                            msg.public_key,
                            msg.address,
                            e
                        );
                    }
                    // The api said the tunnel is gone, so the mirror must not
                    // keep reporting traffic for it even if netlink failed.
                    let mut mirror = self.peers.write().await;
                    peer_plane::remove(&mut mirror, &identity);
                }
                Action::ResetStat => {}
            }
        }

        if authoritative {
            // Mirror first: a peer the api did not list must not keep reporting
            // traffic, and the interface is reconciled against the mirror below.
            let listed: std::collections::HashSet<PeerIdentity> =
                keep.values().flat_map(|set| set.iter().cloned()).collect();
            self.peers
                .write()
                .await
                .retain(|identity, _| listed.contains(identity));
            // The whole interface, not just the messages in this batch: a node
            // that missed live updates converges here, without a manual
            // interface restart.
            self.reconcile_peers_with_mirror().await;
        }

        self.snapshot_wake.notify_one();
        Ok(())
    }

    /// Remove connections of `tag` that the api did not list, with their peers.
    ///
    /// Goes through the regular Delete path so each protocol's cleanup (memory,
    /// WireGuard peer, AmneziaWG peer, Xray handler) stays in one place.
    async fn prune_tag(
        &self,
        tag: Tag,
        keep: &std::collections::HashSet<uuid::Uuid>,
    ) -> Result<usize> {
        let stale: Vec<Message> = {
            let mem = self.memory.read().await;
            mem.iter()
                .filter(|(id, conn)| conn.get_proto().proto() == tag && !keep.contains(*id))
                .map(|(id, conn)| {
                    // Built from the base getters: the `as_*_message` helpers live
                    // on `Conn`, and the node stores a transport-agnostic wrapper.
                    let wg = match tag {
                        Tag::Wireguard => conn.get_wireguard().cloned(),
                        _ => conn.get_amneziawg().cloned(),
                    };
                    Message {
                        conn_id: *id,
                        action: Action::Delete,
                        tag,
                        wg,
                        password: conn.get_password(),
                        token: conn.get_token(),
                        expires_at: conn.get_expires_at().map(Into::into),
                        subscription_id: conn.get_subscription_id(),
                    }
                })
                .collect()
        };

        let mut removed = 0usize;
        for msg in stale {
            match self.handle_message(msg).await {
                Ok(()) => removed += 1,
                Err(e) => tracing::warn!("prune: cannot remove a stale connection: {}", e),
            }
        }
        Ok(removed)
    }

    async fn handle_message(&self, msg: Message) -> Result<()> {
        match msg.action {
            Action::Create | Action::Update => {
                #[cfg(any(feature = "xray", feature = "wireguard", feature = "amnezia-wg"))]
                let conn_id: uuid::Uuid = msg.conn_id;

                match msg.tag {
                    #[cfg(feature = "amnezia-wg")]
                    Tag::AmneziaWg | Tag::AmneziaWgMobile => {
                        let awg = msg.wg.clone().ok_or_else(|| {
                            Error::Custom("Missing Amnezia WireGuard keys".into())
                        })?;

                        let proto = if msg.tag == Tag::AmneziaWgMobile {
                            Proto::new_awg_mobile(&awg)
                        } else {
                            Proto::new_awg(&awg)
                        };

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

                        // Route peer operations to the interface matching the
                        // connection tag: AmneziaWg -> awg0, AmneziaWgMobile -> awg1.
                        let awg_api = match msg.tag {
                            Tag::AmneziaWgMobile => self.awg_mobile_client.as_ref(),
                            _ => self.awg_client.as_ref(),
                        }
                        .ok_or_else(|| {
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
            matches!(attr, AmneziaWireguardPeerAttribute::PublicKey(k) if k == &pubkey)
        })
                        });

                        if exists {
                            return Err(Error::Custom("AWG user already exists".into()));
                        }

                        let peer = AmneziaWireguardPeer(vec![
                            AmneziaWireguardPeerAttribute::PublicKey(pubkey),
                            AmneziaWireguardPeerAttribute::AllowedIps(vec![awg.address.into()]),
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

                        // The peer key is the client-provided public key when
                        // present, otherwise the key derived from the
                        // server-side keypair.
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
                    | Tag::VlessXhttpCdn
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
                    Tag::AmneziaWg | Tag::AmneziaWgMobile => {
                        let awg_api = match tag {
                            Tag::AmneziaWgMobile => self.awg_mobile_client.as_ref(),
                            _ => self.awg_client.as_ref(),
                        }
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
                    | Tag::VlessXhttpCdn
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
        #[cfg(feature = "amnezia-wg")]
        if self.awg_client.is_some() {
            self.collect_awg_metrics().await;
        }
        #[cfg(feature = "amnezia-wg")]
        if self.awg_mobile_client.is_some() {
            self.collect_awg_mobile_metrics().await;
        }
        #[cfg(feature = "amnezia-wg")]
        if self.awg_peers_client.is_some() {
            self.collect_peer_metrics(Tag::AmneziaPeer, "amnezia_peer")
                .await;
        }
        #[cfg(feature = "wireguard")]
        if self.wg_peers_client.is_some() {
            self.collect_peer_metrics(Tag::WireguardPeer, "wireguard_peer")
                .await;
        }
    }
}

/// Peer plane helpers: program the interface, never decide anything.
///
/// The api owns the address book; a node only installs the set of addresses
/// the mirror holds for one public key. The unit of identity is
/// `(public_key, address)`: one key may hold an address per pool, and the
/// kernel keeps them as several allowed-ips of a single peer. Every function is
/// idempotent the way the api expects: a create that is already applied changes
/// nothing, a delete of a missing peer is not an error.
impl<C> Node<C>
where
    C: ConnectionBaseOperations + Send + Sync + Clone + 'static + From<Connection>,
{
    /// Bring every peer interface this node serves in line with the mirror.
    ///
    /// Applying live messages one by one is not enough: a message the node did
    /// not hear (it started after the api published, or its subscriber was not
    /// attached yet) leaves the interface without tunnels until somebody
    /// restarts the interface itself. This compares the whole interface with
    /// the authoritative set — add what is missing, rewrite changed
    /// allowed-ips, remove what the api no longer lists.
    pub(crate) async fn reconcile_peers_with_mirror(&self) {
        #[cfg(feature = "amnezia-wg")]
        self.reconcile_tag(Tag::AmneziaPeer).await;
        #[cfg(feature = "wireguard")]
        self.reconcile_tag(Tag::WireguardPeer).await;
    }

    #[cfg_attr(
        not(any(feature = "amnezia-wg", feature = "wireguard")),
        allow(dead_code)
    )]
    async fn reconcile_tag(&self, tag: Tag) {
        let desired = {
            let mirror = self.peers.read().await;
            peer_plane::desired_by_key(&mirror, tag)
        };
        if let Err(e) = self.reconcile_against(tag, &desired) {
            tracing::error!("peers reconcile: {:?} failed: {}", tag, e);
        }
    }

    /// Diff the interface against `desired` and apply only the differences.
    fn reconcile_against(
        &self,
        tag: Tag,
        desired: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    ) -> Result<()> {
        let current = self.kernel_allowed_map(tag)?;
        let change = peer_plane::diff(&current, desired);
        if change.is_noop() {
            return Ok(());
        }
        tracing::info!(
            "peers reconcile: +{} ~{} -{} ({:?})",
            change.add.len(),
            change.update.len(),
            change.remove.len(),
            tag
        );

        for key in change.add.iter().chain(change.update.iter()) {
            let wanted: Vec<String> = desired
                .get(key)
                .map(|addresses| addresses.iter().cloned().collect())
                .unwrap_or_default();
            let ips_change = if wanted.is_empty() {
                peer_plane::AllowedIpsChange::Remove
            } else {
                peer_plane::AllowedIpsChange::Set(wanted)
            };
            if let Err(e) = self.apply_peer_change(tag, key, ips_change) {
                tracing::error!("peers reconcile: {} {:?} failed: {}", key, tag, e);
            }
        }
        for key in &change.remove {
            if let Err(e) = self.apply_peer_change(tag, key, peer_plane::AllowedIpsChange::Remove) {
                tracing::error!("peers reconcile: removing {} {:?} failed: {}", key, tag, e);
            }
        }
        Ok(())
    }

    /// Make one key of one interface allow exactly `desired`.
    ///
    /// Appending only when addresses are missing keeps a live peer's handshake
    /// and counters; a rewrite happens only when an address has to go, so
    /// revoking one pool does not reset the other.
    #[allow(unused_variables)]
    pub(crate) fn reconcile_peer(
        &self,
        tag: Tag,
        public_key: &str,
        desired: &std::collections::BTreeSet<String>,
    ) -> Result<()> {
        let current = self
            .kernel_allowed_map(tag)?
            .remove(public_key)
            .unwrap_or_default();
        self.apply_peer_change(tag, public_key, peer_plane::plan(&current, desired))
    }

    /// Run one precomputed change against the interface.
    #[allow(unused_variables)]
    fn apply_peer_change(
        &self,
        tag: Tag,
        public_key: &str,
        change: peer_plane::AllowedIpsChange,
    ) -> Result<()> {
        if change == peer_plane::AllowedIpsChange::Keep {
            return Ok(());
        }
        match tag {
            #[cfg(feature = "amnezia-wg")]
            Tag::AmneziaPeer => {
                let api = self
                    .awg_peers_client
                    .as_ref()
                    .ok_or_else(|| Error::Custom("AWG peers interface is not configured".into()))?;
                let key = AwgInterface::decode_pubkey(public_key)?;

                match change {
                    peer_plane::AllowedIpsChange::Keep => Ok(()),
                    peer_plane::AllowedIpsChange::Remove => {
                        // Removing a peer that is already gone is not an error:
                        // the same delete may arrive twice (revoke + device
                        // delete).
                        let _ = api.remove_peer(key);
                        Ok(())
                    }
                    peer_plane::AllowedIpsChange::Add(addresses) => {
                        let peer = peer_plane::awg_peer_with_ips(key, &addresses, false)
                            .map_err(Error::Custom)?;
                        api.add_peer(peer).map_err(|e| {
                            Error::Custom(format!("Failed to add AWG peer via netlink: {}", e))
                        })
                    }
                    peer_plane::AllowedIpsChange::Set(addresses) => {
                        let peer = peer_plane::awg_peer_with_ips(key, &addresses, true)
                            .map_err(Error::Custom)?;
                        api.add_peer(peer).map_err(|e| {
                            Error::Custom(format!("Failed to set AWG allowed-ips: {}", e))
                        })
                    }
                }
            }
            #[cfg(feature = "wireguard")]
            Tag::WireguardPeer => {
                let api = self
                    .wg_peers_client
                    .as_ref()
                    .ok_or_else(|| Error::Custom("WG peers interface is not configured".into()))?;

                match change {
                    peer_plane::AllowedIpsChange::Keep => Ok(()),
                    peer_plane::AllowedIpsChange::Remove => {
                        if api.is_exist(public_key.to_string()) {
                            api.delete(public_key)?;
                        }
                        Ok(())
                    }
                    // defguard rewrites allowed-ips as a set, so an append is
                    // the same call with the union.
                    peer_plane::AllowedIpsChange::Add(addresses)
                    | peer_plane::AllowedIpsChange::Set(addresses) => {
                        let ips = addresses
                            .iter()
                            .map(|address| {
                                address
                                    .parse::<std::net::IpAddr>()
                                    .map(fcore::IpAddrMask::host)
                                    .map_err(|_| {
                                        Error::Custom(format!("Invalid peer address '{}'", address))
                                    })
                            })
                            .collect::<Result<Vec<_>>>()?;
                        api.configure_ips(public_key, &ips)
                    }
                }
            }
            other => Err(Error::Custom(format!(
                "peer message for a non-peer tag: {:?}",
                other
            ))),
        }
    }

    /// What the interface currently holds: `public key -> allowed addresses`.
    #[allow(unused_variables)]
    fn kernel_allowed_map(
        &self,
        tag: Tag,
    ) -> Result<std::collections::BTreeMap<String, std::collections::BTreeSet<String>>> {
        match tag {
            #[cfg(feature = "amnezia-wg")]
            Tag::AmneziaPeer => {
                use base64::Engine;

                let Some(api) = self.awg_peers_client.as_ref() else {
                    return Ok(Default::default());
                };
                let device = api.get_device().map_err(|e| {
                    Error::Custom(format!("Failed to read AWG peers device: {}", e))
                })?;
                Ok(device
                    .peers
                    .iter()
                    .filter_map(|peer| {
                        peer_plane::awg_peer_key(peer).map(|key| {
                            (
                                base64::engine::general_purpose::STANDARD.encode(key),
                                peer_plane::awg_allowed_hosts(peer),
                            )
                        })
                    })
                    .collect())
            }
            #[cfg(feature = "wireguard")]
            Tag::WireguardPeer => {
                let Some(api) = self.wg_peers_client.as_ref() else {
                    return Ok(Default::default());
                };
                let mut out = std::collections::BTreeMap::new();
                for key in api.peer_pubkeys()? {
                    let addresses = api
                        .allowed_ips(&key)?
                        .iter()
                        .map(|ip| ip.address.to_string())
                        .collect();
                    out.insert(key, addresses);
                }
                Ok(out)
            }
            other => Err(Error::Custom(format!(
                "peer message for a non-peer tag: {:?}",
                other
            ))),
        }
    }
}
