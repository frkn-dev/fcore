use crate::config::inbound::StreamSettings;
use crate::get_uuid_last_octet_simple;
use serde::Serialize;

use super::inbound::Inbound;
use super::inbound::Network;
use crate::memory::tag::ProtoTag as Tag;

#[derive(Serialize)]
pub struct ClashConfig {
    port: u16,
    mode: &'static str,
    proxies: Vec<ClashProxy>,
    #[serde(rename = "proxy-groups")]
    proxy_groups: Vec<ClashProxyGroup>,
    rules: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
pub enum ClashProxy {
    Vmess {
        name: String,
        server: String,
        port: u16,
        uuid: String,
        cipher: String,
        udp: bool,
        #[serde(rename = "alterId")]
        alter_id: u8,
        network: String,
        #[serde(rename = "http-opts")]
        http_opts: HttpOpts,
    },
    Vless {
        name: String,
        server: String,
        port: u16,
        uuid: String,
        udp: bool,
        tls: bool,
        network: String,
        servername: String,
        #[serde(rename = "reality-opts", skip_serializing_if = "Option::is_none")]
        reality_opts: Option<RealityOpts>,
        #[serde(rename = "grpc-opts", skip_serializing_if = "Option::is_none")]
        grpc_opts: Option<GrpcOpts>,
        #[serde(rename = "http-opts", skip_serializing_if = "Option::is_none")]
        http_opts: Option<XHttpOpts>,
        #[serde(rename = "client-fingerprint")]
        client_fingerprint: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        flow: Option<String>,
    },
}

#[derive(Serialize)]
pub struct HttpOpts {
    method: &'static str,
    path: Vec<String>,
    headers: HttpHeaders,
    #[serde(rename = "ip-version")]
    ip_version: &'static str,
    host: String,
}

#[derive(Serialize)]
pub struct XHttpOpts {
    path: Vec<String>,
}

#[derive(Serialize, Default)]
pub struct HttpHeaders {
    connection: Vec<&'static str>,
}

#[derive(Serialize)]
pub struct RealityOpts {
    #[serde(rename = "public-key")]
    public_key: String,
    #[serde(rename = "short-id")]
    short_id: String,
}

#[derive(Serialize)]
pub struct GrpcOpts {
    #[serde(rename = "grpc-service-name")]
    grpc_service_name: String,
    #[serde(rename = "ip-version")]
    ip_version: &'static str,
}

#[derive(Serialize)]
pub struct ClashProxyGroup {
    name: String,
    #[serde(rename = "type")]
    group_type: String,
    url: String,
    interval: u32,
    proxies: Vec<String>,
}

pub trait InboundClashConfig {
    fn proxy(
        &self,
        conn_id: &uuid::Uuid,
        _hostname: &str,
        host: &str,
        label: &str,
    ) -> Option<ClashProxy>;
    fn clash(proxies: Vec<ClashProxy>) -> ClashConfig;
}

impl InboundClashConfig for Inbound {
    fn clash(proxies: Vec<ClashProxy>) -> ClashConfig {
        let proxy_names = proxies
            .iter()
            .map(|proxy| match proxy {
                ClashProxy::Vmess { name, .. } => name.clone(),
                ClashProxy::Vless { name, .. } => name.clone(),
            })
            .collect();

        ClashConfig {
            port: 7890,
            mode: "global",
            proxies,
            proxy_groups: vec![ClashProxyGroup {
                name: "♻️ Automatic".into(),
                group_type: "url-test".into(),
                url: "http://www.gstatic.com/generate_204".into(),
                interval: 300,
                proxies: proxy_names,
            }],
            rules: vec![],
        }
    }

    fn proxy(
        &self,
        conn_id: &uuid::Uuid,
        _hostname: &str,
        host: &str,
        label: &str,
    ) -> Option<ClashProxy> {
        let port = self.port;
        let stream = self.stream_settings.as_ref()?;

        let proxy = match self.tag {
            Tag::Vmess => {
                let tcp = stream.tcp_settings.as_ref()?;
                let header = tcp.header.as_ref()?;
                let req = header.request.as_ref()?;
                let host = req.headers.as_ref()?.get("Host")?.first()?.clone();
                let path = req.path.first().cloned().unwrap_or_else(|| "/".to_string());

                let prefix = get_uuid_last_octet_simple(conn_id);

                let name = format!("{} [{}] {}", label, self.tag, prefix);

                Some(ClashProxy::Vmess {
                    name,
                    server: host.to_string(),
                    port,
                    uuid: conn_id.to_string(),
                    cipher: "auto".to_string(),
                    udp: true,
                    alter_id: 0,
                    network: "http".to_string(),
                    http_opts: HttpOpts {
                        method: "GET",
                        path: vec![path],
                        headers: HttpHeaders {
                            connection: vec!["keep-alive"],
                        },
                        ip_version: "dual",
                        host,
                    },
                })
            }
            Tag::VlessGrpcReality | Tag::VlessTcpReality | Tag::VlessXhttpReality => {
                let reality = stream.reality_settings.as_ref()?;

                let (network, grpc_opts, flow, http_opts) = match stream {
                    StreamSettings {
                        grpc_settings: Some(grpc),
                        ..
                    } => (
                        "grpc".to_string(),
                        Some(GrpcOpts {
                            grpc_service_name: grpc.service_name.clone(),
                            ip_version: "dual",
                        }),
                        None,
                        None,
                    ),
                    StreamSettings {
                        xhttp_settings: Some(xhttp),
                        reality_settings: Some(_reality),
                        ..
                    } => (
                        "xhttp".to_string(),
                        None,
                        None,
                        Some(XHttpOpts {
                            path: vec![xhttp.path.clone()],
                        }),
                    ),
                    StreamSettings {
                        network: Network::Tcp,
                        ..
                    } => (
                        "tcp".to_string(),
                        None,
                        Some("xtls-rprx-vision".to_string()),
                        None,
                    ),
                    _ => return None,
                };

                let prefix = get_uuid_last_octet_simple(conn_id);
                let name = format!("{} [{}] {}", label, self.tag, prefix);

                Some(ClashProxy::Vless {
                    name,
                    server: host.to_string(),
                    port,
                    uuid: conn_id.to_string(),
                    udp: true,
                    tls: true,
                    network,
                    servername: reality.server_names.first().cloned().unwrap_or_default(),
                    client_fingerprint: "chrome".to_string(),
                    reality_opts: Some(RealityOpts {
                        public_key: reality.public_key.clone(),
                        short_id: reality.short_ids.first().cloned().unwrap_or_default(),
                    }),
                    grpc_opts,
                    http_opts,
                    flow,
                })
            }

            Tag::VlessXhttpCdn => {
                let xhttp = stream.xhttp_settings.as_ref()?;

                let cdn_host = stream
                    .tls_settings
                    .as_ref()
                    .and_then(|t| t.server_name.clone())
                    .unwrap_or_else(|| host.to_string());

                let prefix = get_uuid_last_octet_simple(conn_id);
                let name = format!("{} [{}] {}", label, self.tag, prefix);

                Some(ClashProxy::Vless {
                    name,
                    server: cdn_host.clone(),
                    port,
                    uuid: conn_id.to_string(),
                    udp: true,
                    tls: true,
                    network: "xhttp".to_string(),
                    servername: cdn_host,
                    client_fingerprint: "chrome".to_string(),
                    reality_opts: None,
                    grpc_opts: None,
                    http_opts: Some(XHttpOpts {
                        path: vec![xhttp.path.clone()],
                    }),
                    flow: None,
                })
            }

            _ => return None,
        };

        proxy
    }
}
