use base64::Engine;
use defguard_wireguard_rs::net::IpAddrMask;
use serde::Serialize;
use std::net::Ipv4Addr;
use url::Url;

use crate::config::xray::Inbound;
use crate::error::{Error, Result};
use crate::memory::tag::ProtoTag as Tag;
use crate::utils::get_uuid_last_octet_simple;

#[derive(Serialize)]
struct VmessConnection {
    v: String,
    ps: String,
    add: String,
    port: String,
    id: String,
    aid: String,
    scy: String,
    net: String,
    r#type: String,
    host: String,
    path: String,
    tls: String,
}

pub fn vmess(
    conn_id: &uuid::Uuid,
    ipv4: Ipv4Addr,
    inbound: Inbound,
    label: &str,
) -> Result<String> {
    let port = inbound.port;
    let stream_settings = inbound
        .stream_settings
        .ok_or(Error::Custom("VMESS: stream settings error".into()))?;
    let tcp_settings = stream_settings
        .tcp_settings
        .ok_or(Error::Custom("VMESS: stream tcp settings error".into()))?;
    let header = tcp_settings
        .header
        .ok_or(Error::Custom("VMESS: header tcp settings error".into()))?;
    let req = header
        .request
        .ok_or(Error::Custom("VMESS: header req settings error".into()))?;
    let headers = req
        .headers
        .ok_or(Error::Custom("VMESS: headers settings error".into()))?;

    let host = headers
        .get("Host")
        .ok_or(Error::Custom("VMESS: host settings error".into()))?
        .first()
        .ok_or(Error::Custom("VMESS: Host stream settings error".into()))?;
    let path = req
        .path
        .first()
        .ok_or(Error::Custom("VMESS: path settings error".into()))?;

    let conn = VmessConnection {
        v: "2".into(),
        ps: format!("Vmess {}", label),
        add: ipv4.to_string(),
        port: port.to_string(),
        id: conn_id.to_string(),
        aid: "0".into(),
        scy: "auto".into(),
        net: "tcp".into(),
        r#type: "http".into(),
        host: host.to_string(),
        path: path.to_string(),
        tls: "none".into(),
    };

    let json_str = serde_json::to_string(&conn)
        .ok()
        .ok_or(Error::Custom("VMESS serde json error".into()))?;
    let base64_str = base64::engine::general_purpose::STANDARD.encode(json_str);

    Ok(format!("vmess://{base64_str}#{label} ____"))
}

pub fn vless_xtls(
    conn_id: &uuid::Uuid,
    ipv4: Ipv4Addr,
    inbound: Inbound,
    label: &str,
) -> Result<String> {
    let port = inbound.port;
    let stream_settings = inbound
        .stream_settings
        .ok_or(Error::Custom("VLESS XTLS: stream settings error".into()))?;
    let reality_settings = stream_settings
        .reality_settings
        .ok_or(Error::Custom("VLESS XTLS: reality settings error".into()))?;
    let pbk = reality_settings.public_key;
    let sid = reality_settings.short_ids.first().ok_or(Error::Custom(
        "VLESS XTLS: reality settings SID error".into(),
    ))?;
    let sni = reality_settings.server_names.first().ok_or(Error::Custom(
        "VLESS XTLS: reality settings SNI error".into(),
    ))?;

    let mut url = Url::parse(&format!("vless://{conn_id}@{ipv4}:{port}"))?;
    url.query_pairs_mut()
        .append_pair("security", "reality")
        .append_pair("flow", "xtls-rprx-vision")
        .append_pair("type", "tcp")
        .append_pair("sni", sni)
        .append_pair("fp", "chrome")
        .append_pair("pbk", &pbk)
        .append_pair("sid", sid);

    let last = get_uuid_last_octet_simple(conn_id);
    url.set_fragment(Some(&format!("{} | {} XTLS", label, last)));

    Ok(url.to_string())
}

pub fn vless_grpc(
    conn_id: &uuid::Uuid,
    ipv4: Ipv4Addr,
    inbound: Inbound,
    label: &str,
) -> Result<String> {
    let port = inbound.port;
    let stream_settings = inbound
        .stream_settings
        .ok_or(Error::Custom("VLESS GRPC: stream settings error".into()))?;
    let reality_settings = stream_settings
        .reality_settings
        .ok_or(Error::Custom("VLESS GRPC: reality settings error".into()))?;
    let grpc_settings = stream_settings
        .grpc_settings
        .ok_or(Error::Custom("VLESS GRPC: grpc settings error".into()))?;
    let service_name = grpc_settings.service_name;
    let pbk = reality_settings.public_key;
    let sid = reality_settings.short_ids.first().ok_or(Error::Custom(
        "VLESS GRPC: reality settings SID error".into(),
    ))?;
    let sni = reality_settings.server_names.first().ok_or(Error::Custom(
        "VLESS GRPC: reality settings SNI error".into(),
    ))?;

    let mut url = Url::parse(&format!("vless://{conn_id}@{ipv4}:{port}"))?;

    url.query_pairs_mut()
        .append_pair("security", "reality")
        .append_pair("type", "grpc")
        .append_pair("mode", "gun")
        .append_pair("serviceName", &service_name)
        .append_pair("fp", "chrome")
        .append_pair("sni", sni)
        .append_pair("pbk", &pbk)
        .append_pair("sid", sid);

    let last = get_uuid_last_octet_simple(conn_id);
    url.set_fragment(Some(&format!("{} | {} GRPC", label, last)));

    Ok(url.to_string())
}

pub fn vless_xhttp(
    conn_id: &uuid::Uuid,
    ipv4: Ipv4Addr,
    inbound: Inbound,
    label: &str,
) -> Result<String> {
    let port = inbound.port;
    let stream_settings = inbound
        .stream_settings
        .ok_or(Error::Custom("VLESS XHTTP: stream settings error".into()))?;
    let reality_settings = stream_settings
        .reality_settings
        .ok_or(Error::Custom("VLESS XHTTP: reality settings error".into()))?;
    let xhttp_settings = stream_settings
        .xhttp_settings
        .ok_or(Error::Custom("VLESS XHTTP: xhttp settings error".into()))?;

    let path = xhttp_settings.path;
    let pbk = reality_settings.public_key;
    let sid = reality_settings.short_ids.first().ok_or(Error::Custom(
        "VLESS XHTTP: reality settings SID error".into(),
    ))?;
    let sni = reality_settings.server_names.first().ok_or(Error::Custom(
        "VLESS XHTTP: reality settings SNI error".into(),
    ))?;

    let mut url = Url::parse(&format!("vless://{conn_id}@{ipv4}:{port}"))?;

    url.query_pairs_mut()
        .append_pair("security", "reality")
        .append_pair("type", "xhttp")
        .append_pair("path", &path)
        .append_pair("fp", "chrome")
        .append_pair("sni", sni)
        .append_pair("pbk", &pbk)
        .append_pair("sid", sid);

    let last = get_uuid_last_octet_simple(conn_id);
    url.set_fragment(Some(&format!("{} | {} XHTTP", label, last)));

    Ok(url.to_string())
}

pub fn h2(inbound: &Inbound, label: &str, token: &Option<uuid::Uuid>) -> Result<String> {
    let port = inbound.port;
    let h2 = inbound
        .h2
        .as_ref()
        .ok_or(Error::Custom("Hysteria2: H2Settings missing".to_string()))?;

    if let Some(inb) = &inbound.h2 {
        let hostname = inb.host.clone();

        let _alpn = h2.alpn.as_ref().map(|v| v.join(",")).unwrap_or_default();

        if let Some(token) = token {
            let mut url = Url::parse(&format!("hysteria2://{token}@{hostname}:{port}"))?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("host", &h2.host)
                    .append_pair("insecure", &h2.insecure.to_string())
                    .append_pair("up-mbps", &h2.up_mbps.unwrap_or(0).to_string())
                    .append_pair("down-mbps", &h2.down_mbps.unwrap_or(0).to_string());
                // Salamander PSK has to travel with the link: a client that dials
                // the node without it gets a silent QUIC timeout, not an auth error.
                if let Some(obfs) = h2.obfs.as_ref().filter(|o| !o.r#type.is_empty()) {
                    q.append_pair("obfs", &obfs.r#type);
                    if !obfs.password.is_empty() {
                        q.append_pair("obfs-password", &obfs.password);
                    }
                }
            }

            let last = get_uuid_last_octet_simple(token);
            url.set_fragment(Some(&format!("{} | {} H2", label, last)));

            Ok(url.to_string())
        } else {
            Err(Error::Custom("Token is not valid".to_string()))
        }
    } else {
        Err(Error::Custom("H2 Inbound is not valid".to_string()))
    }
}

pub fn mtproto(address: Ipv4Addr, inbound: &Inbound, label: &str) -> Result<String> {
    let port = inbound.port;

    let secret = inbound
        .mtproto_secret
        .as_ref()
        .ok_or(Error::Custom("MTProto: secret missing".to_string()))?;

    let mut url = Url::parse(&format!(
        "https://t.me/proxy?server={address}&port={port}&secret={secret}"
    ))?;

    url.set_fragment(Some(label));

    Ok(url.to_string())
}

pub fn create_conn_link(
    tag: Tag,
    conn_id: &uuid::Uuid,
    inbound: &Inbound,
    label: &str,
    address: Ipv4Addr,
    token: &Option<uuid::Uuid>,
) -> Result<String> {
    let raw_link = match tag {
        Tag::VlessTcpReality => vless_xtls(conn_id, address, inbound.clone(), label),
        Tag::VlessGrpcReality => vless_grpc(conn_id, address, inbound.clone(), label),
        Tag::VlessXhttpReality => vless_xhttp(conn_id, address, inbound.clone(), label),
        Tag::Hysteria2 => h2(&inbound.clone(), label, token),
        Tag::Vmess => vmess(conn_id, address, inbound.clone(), label),
        _ => return Err(Error::Custom("Cannot complete conn line".into())),
    }?;

    let parsed =
        Url::parse(&raw_link).map_err(|_| Error::Custom("Invalid URL generated".into()))?;

    Ok(parsed.to_string())
}

pub fn wireguard_conn(
    conn_id: &uuid::Uuid,
    ipv4: &Ipv4Addr,
    inbound: Inbound,
    label: &str,
    private_key: &str,
    client_ip: &IpAddrMask,
) -> Result<String> {
    if let Some(wg) = inbound.wg {
        let server_pubkey = wg.pubkey;
        let host = ipv4;
        let port = wg.port;
        let dns: Vec<_> = wg.dns.iter().map(|d| d.to_string()).collect();
        let dns = dns.join(",");

        let config = format!(
            r#"
[Interface]
PrivateKey = {private_key}
Address    = {client_ip}
DNS        = {dns}

[Peer]
PublicKey           = {server_pubkey}
Endpoint            = {host}:{port}
AllowedIPs          = 0.0.0.0/0, ::/0
PersistentKeepalive = 25

# {label} — conn_id: {conn_id}"#
        );

        Ok(config)
    } else {
        Err(Error::Custom("WG is not configured".into()))
    }
}
