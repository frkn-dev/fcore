use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{fs::File, io::Read};
use url::Url;

use crate::error::{Error, Result};
use crate::memory::connection::conn::Conn as Connection;
use crate::memory::connection::operation::base::Operations;
use crate::memory::tag::ProtoTag as Tag;
use crate::utils::get_uuid_last_octet_simple;

use crate::config::amnezia_wg::AmneziaWgSettings;
use crate::config::h2::H2Settings;
use crate::config::wireguard::WireguardSettings;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Xhttp,
    Grpc,
    Tcp,
    Ws,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StreamSettings {
    pub network: Network,
    #[serde(rename = "tcpSettings")]
    pub tcp_settings: Option<TcpSettings>,
    #[serde(rename = "realitySettings")]
    pub reality_settings: Option<RealitySettings>,
    #[serde(rename = "grpcSettings")]
    pub grpc_settings: Option<GrpcSettings>,
    #[serde(rename = "xhttpSettings")]
    pub xhttp_settings: Option<XhttpSettings>,
    #[serde(rename = "tlsSettings")]
    pub tls_settings: Option<TlsSettings>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GrpcSettings {
    #[serde(rename = "serviceName")]
    pub service_name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct RealitySettings {
    #[serde(rename = "serverNames")]
    pub server_names: Vec<String>,
    #[serde(rename = "privateKey")]
    pub private_key: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    #[serde(rename = "shortIds")]
    pub short_ids: Vec<String>,
    pub target: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct XhttpSettings {
    pub path: String,
    pub mode: Option<String>,
    pub extra: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TlsSettings {
    #[serde(rename = "serverName")]
    pub server_name: Option<String>,
    pub certificates: Vec<Certificate>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Certificate {
    #[serde(rename = "certificateFile")]
    pub certificate_file: String,
    #[serde(rename = "keyFile")]
    pub key_file: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TcpSettings {
    pub header: Option<TcpHeader>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TcpHeader {
    pub r#type: String,
    pub request: Option<TcpRequest>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TcpRequest {
    pub method: String,
    pub path: Vec<String>,
    pub headers: Option<std::collections::HashMap<String, Vec<String>>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Inbound {
    pub tag: Tag,
    pub port: u16,
    #[serde(rename = "streamSettings")]
    pub stream_settings: Option<StreamSettings>,
    pub wg: Option<WireguardSettings>,
    pub awg: Option<AmneziaWgSettings>,
    pub h2: Option<H2Settings>,
    pub mtproto_secret: Option<String>,
}

impl Inbound {
    pub fn as_inbound_response(&self) -> InboundResponse {
        InboundResponse {
            port: self.port,
            stream_settings: self.stream_settings.clone(),
            tag: self.tag,
            wg: self.wg.clone(),
            awg: self.awg.clone(),
            h2: self.h2.clone(),
            mtproto_secret: self.mtproto_secret.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InboundResponse {
    pub tag: Tag,
    pub port: u16,
    pub stream_settings: Option<StreamSettings>,
    pub wg: Option<WireguardSettings>,
    pub awg: Option<AmneziaWgSettings>,
    pub h2: Option<H2Settings>,
    pub mtproto_secret: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Api {
    pub listen: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Settings {
    pub inbounds: Vec<Inbound>,
    pub api: Api,
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        Ok(())
    }

    pub fn from_file(file_path: &str) -> Result<Settings> {
        let mut file = File::open(file_path)?;
        let mut contents = String::new();

        file.read_to_string(&mut contents)?;

        let settings: Settings = serde_json::from_str(&contents)?;

        Ok(settings)
    }
}

pub trait InboundConnLink {
    fn create_link(
        &self,
        conn_id: &uuid::Uuid,
        conn: &Connection,
        hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String>;
    fn vless_xtls(
        &self,
        conn_id: &uuid::Uuid,
        hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String>;
    fn vless_grpc(
        &self,
        conn_id: &uuid::Uuid,
        hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String>;
    fn vless_xhttp(
        &self,
        conn_id: &uuid::Uuid,
        hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String>;
    fn vless_xhttp_cdn(
        &self,
        conn_id: &uuid::Uuid,
        hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String>;
    fn h2(&self, hostname: &str, label: &str, conn: &Connection) -> Result<String>;
    fn vmess(
        &self,
        conn_id: &uuid::Uuid,
        hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String>;
    fn mtproto(&self, hostname: &str, host: &str, label: &str) -> Result<String>;
    fn wireguard(
        &self,
        conn_id: &uuid::Uuid,
        conn: &Connection,
        hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String>;
    fn amneziawg(
        &self,
        conn_id: &uuid::Uuid,
        conn: &Connection,
        _hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String>;
}

impl InboundConnLink for Inbound {
    fn create_link(
        &self,
        conn_id: &uuid::Uuid,
        conn: &Connection,
        hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String> {
        match self.tag {
            Tag::VlessTcpReality => self.vless_xtls(conn_id, hostname, host, label),
            Tag::VlessGrpcReality => self.vless_grpc(conn_id, hostname, host, label),
            Tag::VlessXhttpReality => self.vless_xhttp(conn_id, hostname, host, label),
            Tag::VlessXhttpCdn => self.vless_xhttp_cdn(conn_id, hostname, host, label),
            Tag::Hysteria2 => self.h2(hostname, label, conn),
            Tag::Wireguard => self.wireguard(conn_id, conn, hostname, host, label),
            Tag::AmneziaWg => self.amneziawg(conn_id, conn, hostname, host, label),
            Tag::Mtproto => self.mtproto(hostname, host, label),
            Tag::Vmess => self.vmess(conn_id, hostname, host, label),
            _ => Err(Error::Custom("Unsupported protocol tag".into())),
        }
    }

    fn amneziawg(
        &self,
        conn_id: &uuid::Uuid,
        conn: &Connection,
        _hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String> {
        tracing::debug!("Trying to print AWG conn");

        if let Some(awg_conn) = conn.get_amneziawg() {
            let private_key = awg_conn.keys.privkey.clone();
            let client_ip = awg_conn.address.clone();

            if let Some(awg) = &self.awg {
                let server_pubkey = awg.interface.private_key.pubkey()?;
                let port = awg.interface.listen_port;

                let dns = awg
                    .interface
                    .dns
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(",");

                let mut config = format!(
                    r#"[Interface]
PrivateKey = {private_key}
Address = {client_ip}
"#,
                );

                if let Some(mtu) = awg.interface.mtu {
                    config.push_str(&format!("MTU = {}\n", mtu));
                }

                if !dns.is_empty() {
                    config.push_str(&format!("DNS = {}\n", dns));
                }

                if let Some(obf) = &awg.obfuscation {
                    config.push_str(&format!(
                        r#"
Jc = {}
Jmin = {}
Jmax = {}
S1 = {}
S2 = {}
S3 = {}
S4 = {}
H1 = {}
H2 = {}
H3 = {}
H4 = {}
I1 = {}
I2 = {}
I3 = {}
I4 = {}
I5 = {}
"#,
                        obf.jc,
                        obf.jmin,
                        obf.jmax,
                        obf.s1,
                        obf.s2,
                        obf.s3,
                        obf.s4,
                        obf.h1,
                        obf.h2,
                        obf.h3,
                        obf.h4,
                        obf.i1,
                        obf.i2,
                        obf.i3,
                        obf.i4,
                        obf.i5,
                    ));
                }

                config.push_str(&format!(
                    r#"
[Peer]
PublicKey = {server_pubkey}
Endpoint = {host}:{port}
AllowedIPs = 0.0.0.0/0, ::/0
PersistentKeepalive = 25
"#
                ));

                config.push_str(&format!("\n# {} — conn_id: {}\n", label, conn_id));

                Ok(config)
            } else {
                Err(Error::Custom("AWG Inbound is not configured".into()))
            }
        } else {
            Err(Error::Custom("AWG Conn is not configured".into()))
        }
    }

    fn wireguard(
        &self,
        conn_id: &uuid::Uuid,
        conn: &Connection,
        _hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String> {
        tracing::debug!("Trying to print WG conn");
        if let Some(wg_conn) = conn.get_wireguard() {
            let private_key = wg_conn.keys.privkey.clone();
            let client_ip = wg_conn.address.clone();

            if let Some(wg) = &self.wg {
                let server_pubkey = wg.keys.pubkey()?;
                let port = wg.port;

                let dns = wg
                    .dns
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(",");

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

    # {label} — conn_id: {conn_id}
    "#
                );

                Ok(config)
            } else {
                Err(Error::Custom("WG Inbound is not configured".into()))
            }
        } else {
            Err(Error::Custom("WG Conn is not configured".into()))
        }
    }

    fn vmess(
        &self,
        conn_id: &uuid::Uuid,
        _hostname: &str,
        _host: &str,
        label: &str,
    ) -> Result<String> {
        let port = self.port;
        let stream_settings = self
            .stream_settings
            .clone()
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

        let conn = VmessConnection {
            v: "2".into(),
            ps: format!("Vmess {}", label),
            add: host.to_string(),
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

        Ok(format!("vmess://{base64_str}#{label}"))
    }

    fn vless_xtls(
        &self,
        conn_id: &uuid::Uuid,
        _hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String> {
        let s = self
            .stream_settings
            .as_ref()
            .ok_or(Error::Custom("Missing stream settings".into()))?;
        let r = s
            .reality_settings
            .as_ref()
            .ok_or(Error::Custom("Missing reality settings".into()))?;

        let pbk = &r.public_key;
        let sid = r
            .short_ids
            .first()
            .ok_or(Error::Custom("Missing SID".into()))?;
        let sni = r
            .server_names
            .first()
            .ok_or(Error::Custom("Missing SNI".into()))?;

        let mut url = Url::parse(&format!("vless://{conn_id}@{host}:{}", self.port))?;
        url.query_pairs_mut()
            .append_pair("security", "reality")
            .append_pair("flow", "xtls-rprx-vision")
            .append_pair("type", "tcp")
            .append_pair("sni", sni)
            .append_pair("fp", "chrome")
            .append_pair("pbk", pbk)
            .append_pair("sid", sid);

        url.set_fragment(Some(&format!(
            "{} | {} XTLS",
            label,
            get_uuid_last_octet_simple(conn_id)
        )));
        Ok(url.to_string())
    }

    fn vless_grpc(
        &self,
        conn_id: &uuid::Uuid,
        _hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String> {
        let s = self
            .stream_settings
            .as_ref()
            .ok_or(Error::Custom("Missing stream settings".into()))?;
        let r = s
            .reality_settings
            .as_ref()
            .ok_or(Error::Custom("Missing reality settings".into()))?;
        let g = s
            .grpc_settings
            .as_ref()
            .ok_or(Error::Custom("Missing gRPC settings".into()))?;

        let mut url = Url::parse(&format!("vless://{conn_id}@{host}:{}", self.port))?;
        url.query_pairs_mut()
            .append_pair("security", "reality")
            .append_pair("type", "grpc")
            .append_pair("serviceName", &g.service_name)
            .append_pair("sni", r.server_names.first().unwrap_or(&"".to_string()))
            .append_pair("pbk", &r.public_key)
            .append_pair("sid", r.short_ids.first().unwrap_or(&"".to_string()));

        url.set_fragment(Some(&format!(
            "{} | {} GRPC",
            label,
            get_uuid_last_octet_simple(conn_id)
        )));
        Ok(url.to_string())
    }

    fn vless_xhttp(
        &self,
        conn_id: &uuid::Uuid,
        _hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String> {
        let s = self
            .stream_settings
            .as_ref()
            .ok_or(Error::Custom("Missing stream settings".into()))?;
        let r = s
            .reality_settings
            .as_ref()
            .ok_or(Error::Custom("Missing reality settings".into()))?;
        let x = s
            .xhttp_settings
            .as_ref()
            .ok_or(Error::Custom("Missing xHTTP settings".into()))?;

        let mut url = Url::parse(&format!("vless://{conn_id}@{host}:{}", self.port))?;
        url.query_pairs_mut()
            .append_pair("security", "reality")
            .append_pair("type", "xhttp")
            .append_pair("path", &x.path)
            .append_pair("pbk", &r.public_key);

        url.set_fragment(Some(&format!(
            "{} | {} XHTTP",
            label,
            get_uuid_last_octet_simple(conn_id)
        )));
        Ok(url.to_string())
    }

    fn vless_xhttp_cdn(
        &self,
        conn_id: &uuid::Uuid,
        _hostname: &str,
        host: &str,
        label: &str,
    ) -> Result<String> {
        let s = self
            .stream_settings
            .as_ref()
            .ok_or(Error::Custom("Missing stream settings".into()))?;
        let x = s
            .xhttp_settings
            .as_ref()
            .ok_or(Error::Custom("Missing xHTTP settings".into()))?;

        let cdn_host = s
            .tls_settings
            .as_ref()
            .and_then(|t| t.server_name.clone())
            .unwrap_or_else(|| host.to_string());

        let mut url = Url::parse(&format!("vless://{conn_id}@{cdn_host}:{}", self.port))?;
        let mut builder = url.query_pairs_mut();
        builder
            .append_pair("encryption", "none")
            .append_pair("security", "tls")
            .append_pair("type", "xhttp")
            .append_pair("host", &cdn_host)
            .append_pair("path", &x.path)
            .append_pair("sni", &cdn_host);

        if let Some(mode) = &x.mode {
            builder.append_pair("mode", mode);
        }

        drop(builder);

        if let Some(extra) = &x.extra {
            let extra_json = serde_json::to_string(extra)?;
            url.query_pairs_mut().append_pair("extra", &extra_json);
        }

        url.set_fragment(Some(&format!(
            "{} | {} XHTTP-CDN",
            label,
            get_uuid_last_octet_simple(conn_id)
        )));
        Ok(url.to_string())
    }

    fn h2(&self, _hostname: &str, label: &str, conn: &Connection) -> Result<String> {
        let h2 = self
            .h2
            .as_ref()
            .ok_or(Error::Custom("Hysteria2 settings missing".into()))?;

        if let Some(token) = conn.get_token() {
            let mut url = Url::parse(&format!("hysteria2://{token}@{}:{}", h2.host, self.port))?;
            url.query_pairs_mut()
                .append_pair("insecure", &h2.insecure.to_string())
                .append_pair("up-mbps", &h2.up_mbps.unwrap_or(0).to_string());

            url.set_fragment(Some(&format!(
                "{} | {} H2",
                label,
                get_uuid_last_octet_simple(&token)
            )));
            Ok(url.to_string())
        } else {
            Err(Error::Custom("H2 Token is required".into()))
        }
    }

    fn mtproto(&self, _hostname: &str, host: &str, label: &str) -> Result<String> {
        let port = self.port;

        let secret = self
            .mtproto_secret
            .as_ref()
            .ok_or(Error::Custom("Mtproto settings missing".into()))?;

        let mut url = Url::parse(&format!(
            "https://t.me/proxy?server={host}&port={port}&secret={secret}"
        ))?;

        url.set_fragment(Some(label));

        Ok(url.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vless_xhttp_cdn_link() {
        let inbound = Inbound {
            tag: Tag::VlessXhttpCdn,
            port: 443,
            stream_settings: Some(StreamSettings {
                network: Network::Xhttp,
                tcp_settings: None,
                reality_settings: None,
                grpc_settings: None,
                xhttp_settings: Some(XhttpSettings {
                    path: "/cdn".into(),
                    mode: Some("auto".into()),
                    extra: Some(serde_json::json!({"scMaxEachPostBytes": 1000000})),
                }),
                tls_settings: Some(TlsSettings {
                    server_name: Some("cdn.example.com".into()),
                    certificates: vec![],
                }),
            }),
            wg: None,
            awg: None,
            h2: None,
            mtproto_secret: None,
        };

        let conn_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let link = inbound
            .vless_xhttp_cdn(&conn_id, "node", "node.example.com", "Test")
            .unwrap();

        assert!(
            link.starts_with("vless://550e8400-e29b-41d4-a716-446655440000@cdn.example.com:443?"),
            "unexpected link start: {}",
            link
        );
        assert!(
            link.contains("encryption=none"),
            "missing encryption: {}",
            link
        );
        assert!(link.contains("security=tls"), "missing security: {}", link);
        assert!(link.contains("type=xhttp"), "missing type: {}", link);
        assert!(
            link.contains("host=cdn.example.com"),
            "missing host: {}",
            link
        );
        assert!(link.contains("path=%2Fcdn"), "missing path: {}", link);
        assert!(link.contains("mode=auto"), "missing mode: {}", link);
        assert!(link.contains("extra="), "missing extra: {}", link);
        assert!(
            link.contains("sni=cdn.example.com"),
            "missing sni: {}",
            link
        );
        assert!(
            link.ends_with("#Test%20|%20446655440000%20XHTTP-CDN"),
            "unexpected fragment: {}",
            link
        );
    }
}
