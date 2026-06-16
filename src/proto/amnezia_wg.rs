use crate::error::{Error, Result as MyResult};
use base64::{engine::general_purpose, Engine as _};
use netlink_packet_amnezia_wireguard::{
    AmneziaWireguardAttribute, AmneziaWireguardCmd, AmneziaWireguardMessage,
    AmneziaWireguardPeer, AmneziaWireguardPeerAttribute,
};
use netlink_packet_core::{
    NetlinkDeserializable, NetlinkMessage, NetlinkPayload, NetlinkSerializable, NLM_F_ACK,
    NLM_F_DUMP, NLM_F_REQUEST,
};
use netlink_packet_generic::{
    ctrl::{nlas::GenlCtrlAttrs, GenlCtrl, GenlCtrlCmd},
    GenlFamily, GenlMessage,
};
use netlink_sys::{constants::NETLINK_GENERIC, Socket, SocketAddr};
use std::collections::HashMap;
use std::{fmt::Debug, io::ErrorKind};
use thiserror::Error as ThisError;
use tracing::{error, trace};

type NetlinkResult<T> = Result<T, NetlinkError>;

impl From<NetlinkError> for Error {
    fn from(e: NetlinkError) -> Self {
        Error::Custom(e.to_string())
    }
}

#[derive(Clone)]
pub struct AwgInterface {
    family_id: u16,
    interface: String,
}
#[derive(Debug, Clone)]
pub struct PeerStats {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub last_handshake: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct AwgDevice {
    pub ifname: String,
    pub listen_port: Option<u16>,
    pub peers: Vec<AmneziaWireguardPeer>,
}

const WGPEER_F_REMOVE_ME: u32 = 1;
const SOCKET_BUFFER_LENGTH: usize = 12288;

#[derive(Debug, ThisError)]
pub(crate) enum NetlinkError {
    #[error("Unexpected netlink payload")]
    UnexpectedPayload,
    #[error("Failed to send netlink request")]
    SendFailure,
    #[error("Attribute value not found")]
    AttributeNotFound,
    #[error("Socket error: {0}")]
    SocketError(String),
    #[error("Failed to read response")]
    ResponseError(#[from] netlink_packet_core::DecodeError),
    #[error("Netlink payload error: {0}")]
    PayloadError(netlink_packet_core::ErrorMessage),
    #[error("Failed to create WireGuard interface")]
    CreateInterfaceError,
    #[error("Failed to delete WireGuard interface")]
    DeleteInterfaceError,
    #[error("File already exists")]
    FileAlreadyExists,
    #[error("Add route error")]
    AddRouteError,
    #[error("No such file")]
    NotFound,
    #[error("Failed to add rule")]
    AddRuleError,
    #[error("Failed to delete rule")]
    DeleteRuleError,
}

macro_rules! get_nla_value {
    ($nlas:expr, $e:ident, $v:ident) => {
        $nlas.iter().find_map(|attr| match attr {
            $e::$v(value) => Some(value),
            _ => None,
        })
    };
}

impl AwgInterface {
    pub fn connect(interface: String) -> MyResult<Self> {
        let family_id = resolve_family_id("amneziawg")?;

        Ok(Self {
            family_id,
            interface,
        })
    }

    pub fn decode_pubkey(key: &str) -> Result<[u8; 32], Error> {
        let bytes = general_purpose::STANDARD
            .decode(key.trim())
            .map_err(|e| Error::Custom(format!("bad pubkey base64: {e}")))?;

        if bytes.len() != 32 {
            return Err(Error::Custom("invalid pubkey length".into()));
        }

        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(arr)
    }

    pub fn get_device(&self) -> MyResult<AwgDevice> {
        let ifname = &self.interface;
        let msg = AmneziaWireguardMessage {
            cmd: AmneziaWireguardCmd::GetDevice,
            attributes: vec![AmneziaWireguardAttribute::IfName(ifname.into())],
        };

        let genlmsg = GenlMessage::from_payload(msg);
        let responses = netlink_request_genl(genlmsg, NLM_F_REQUEST | NLM_F_DUMP)?;

        let mut device = AwgDevice {
            ifname: ifname.to_string(),
            listen_port: None,
            peers: Vec::new(),
        };

        for response in responses {
            let attrs = match &response.payload {
                NetlinkPayload::InnerMessage(genl) => &genl.payload.attributes,
                _ => continue,
            };

            for attr in attrs {
                match attr {
                    AmneziaWireguardAttribute::ListenPort(port) => {
                        device.listen_port = Some(*port);
                    }

                    AmneziaWireguardAttribute::Peers(peers) => {
                        device.peers.extend(peers.clone());
                    }

                    _ => {}
                }
            }
        }

        Ok(device)
    }

    pub fn add_peer(&self, peer: AmneziaWireguardPeer) -> MyResult<()> {
        let ifname = &self.interface;
        let msg = AmneziaWireguardMessage {
            cmd: AmneziaWireguardCmd::SetDevice,
            attributes: vec![
                AmneziaWireguardAttribute::IfName(ifname.into()),
                AmneziaWireguardAttribute::Peers(vec![peer]),
            ],
        };

        let genlmsg = GenlMessage::from_payload(msg);
        let responses = netlink_request_genl(genlmsg, NLM_F_REQUEST | NLM_F_ACK)?;

        Ok(())
    }

    pub fn remove_peer(&self, public_key: [u8; 32]) -> MyResult<()> {
        let peer = AmneziaWireguardPeer(vec![
            AmneziaWireguardPeerAttribute::PublicKey(public_key),
            AmneziaWireguardPeerAttribute::Flags(WGPEER_F_REMOVE_ME),
        ]);

        let ifname = &self.interface;

        let msg = AmneziaWireguardMessage {
            cmd: AmneziaWireguardCmd::SetDevice,
            attributes: vec![
                AmneziaWireguardAttribute::IfName(ifname.into()),
                AmneziaWireguardAttribute::Peers(vec![peer]),
            ],
        };

        let genlmsg = GenlMessage::from_payload(msg);
        let responses = netlink_request_genl(genlmsg, NLM_F_REQUEST | NLM_F_ACK)?;

        Ok(())
    }

    pub fn peer_stats(&self) -> MyResult<HashMap<[u8; 32], PeerStats>> {
        let ifname = &self.interface;
        let device = self.get_device()?;

        let mut stats = HashMap::new();

        for peer in device.peers {
            let mut pubkey = None;
            let mut rx = 0;
            let mut tx = 0;
            let mut hs = None;

            for attr in peer.0 {
                match attr {
                    AmneziaWireguardPeerAttribute::PublicKey(key) => {
                        pubkey = Some(key);
                    }

                    AmneziaWireguardPeerAttribute::RxBytes(v) => {
                        rx = v;
                    }

                    AmneziaWireguardPeerAttribute::TxBytes(v) => {
                        tx = v;
                    }

                    AmneziaWireguardPeerAttribute::LastHandshake(time) => {
                        hs = Some((time.seconds * 1_000_000_000 + time.nano_seconds) as u64);
                    }

                    _ => {}
                }
            }

            if let Some(key) = pubkey {
                stats.insert(
                    key,
                    PeerStats {
                        rx_bytes: rx,
                        tx_bytes: tx,
                        last_handshake: hs,
                    },
                );
            }
        }

        Ok(stats)
    }
}

fn resolve_family_id(name: &str) -> MyResult<u16> {
    let genlmsg = GenlMessage::from_payload(GenlCtrl {
        cmd: GenlCtrlCmd::GetFamily,
        nlas: vec![GenlCtrlAttrs::FamilyName(name.to_string())],
    });

    let responses = netlink_request_genl(genlmsg, NLM_F_REQUEST | NLM_F_ACK)?;

    match responses.first() {
        Some(NetlinkMessage {
            payload:
                NetlinkPayload::InnerMessage(GenlMessage {
                    payload: GenlCtrl { nlas, .. },
                    ..
                }),
            ..
        }) => {
            let family_id = get_nla_value!(nlas, GenlCtrlAttrs, FamilyId)
                .ok_or_else(|| Error::Custom("family id not found".to_string()))?;

            Ok(*family_id)
        }

        _ => Err(Error::Custom("unexpected payload".to_string())),
    }
}

fn netlink_request_genl<F>(
    mut message: GenlMessage<F>,
    flags: u16,
) -> NetlinkResult<Vec<NetlinkMessage<GenlMessage<F>>>>
where
    F: GenlFamily + Clone + Debug + Eq,
    GenlMessage<F>: Clone + Debug + Eq + NetlinkSerializable + NetlinkDeserializable,
{
    if message.family_id() == 0 {
        let genlmsg: GenlMessage<GenlCtrl> = GenlMessage::from_payload(GenlCtrl {
            cmd: GenlCtrlCmd::GetFamily,
            nlas: vec![GenlCtrlAttrs::FamilyName(F::family_name().to_string())],
        });
        let responses = netlink_request_genl::<GenlCtrl>(genlmsg, NLM_F_REQUEST | NLM_F_ACK)?;

        match responses.first() {
            Some(NetlinkMessage {
                payload:
                    NetlinkPayload::InnerMessage(GenlMessage {
                        payload: GenlCtrl { nlas, .. },
                        ..
                    }),
                ..
            }) => {
                let family_id = get_nla_value!(nlas, GenlCtrlAttrs, FamilyId)
                    .ok_or_else(|| NetlinkError::AttributeNotFound)?;
                message.set_resolved_family_id(*family_id);
            }
            _ => return Err(NetlinkError::UnexpectedPayload),
        }
    }
    netlink_request(message, flags, NETLINK_GENERIC)
}

fn netlink_request<I>(
    message: I,
    flags: u16,
    protocol: isize,
) -> NetlinkResult<Vec<NetlinkMessage<I>>>
where
    NetlinkPayload<I>: From<I>,
    I: Clone + Debug + Eq + NetlinkSerializable + NetlinkDeserializable,
{
    let mut req = NetlinkMessage::from(message);

    req.header.flags = flags;
    req.finalize();
    let len = req.buffer_len();
    let mut buf = vec![0u8; len];
    req.serialize(&mut buf);

    let socket = Socket::new(protocol).map_err(|err| {
        error!("Failed to open socket: {err}");
        NetlinkError::SocketError(err.to_string())
    })?;
    let kernel_addr = SocketAddr::new(0, 0);
    socket.connect(&kernel_addr).map_err(|err| {
        error!("Failed to connect to socket: {err}");
        NetlinkError::SocketError(err.to_string())
    })?;
    let n_sent = socket.send(&buf, 0).map_err(|err| {
        error!("Failed to send to socket: {err}");
        NetlinkError::SocketError(err.to_string())
    })?;
    if n_sent != len {
        return Err(NetlinkError::SendFailure);
    }

    let mut responses = Vec::new();
    loop {
        let mut recv_buf = [0; SOCKET_BUFFER_LENGTH];
        let n_received = socket.recv(&mut &mut recv_buf[..], 0).map_err(|err| {
            error!("Failed to receive from socket: {err}");
            NetlinkError::SocketError(err.to_string())
        })?;
        let mut offset = 0;
        loop {
            let response = NetlinkMessage::<I>::deserialize(&recv_buf[offset..])?;
            trace!("Read netlink response from socket: {response:?}");
            match response.payload {
                // We've parsed all parts of the response and can leave the loop.
                NetlinkPayload::Error(msg) if msg.code.is_none() => return Ok(responses),
                NetlinkPayload::Done(_) => return Ok(responses),
                NetlinkPayload::Error(msg) => {
                    return match msg.to_io().kind() {
                        ErrorKind::AlreadyExists => Err(NetlinkError::FileAlreadyExists),
                        ErrorKind::NotFound => Err(NetlinkError::NotFound),
                        _ => Err(NetlinkError::PayloadError(msg)),
                    };
                }
                _ => {}
            }
            let header_length = response.header.length as usize;
            offset += header_length;
            responses.push(response);
            if offset == n_received || header_length == 0 {
                // We've fully parsed the datagram, but there may be further datagrams
                // with additional netlink response parts.
                break;
            }
        }
    }
}
