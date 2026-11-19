use crate::config::inbound::StreamSettings;
use crate::get_uuid_last_octet_simple;
use serde::Serialize;

use super::inbound::Inbound;
use super::inbound::Network;
use crate::memory::connection::conn::Conn as Connection;
use crate::memory::connection::operation::base::Operations;
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
        // mihomo reads `xhttp-opts` (string path) for network: xhttp; the old
        // `http-opts` key belongs to network: http and left the node dead.
        #[serde(rename = "xhttp-opts", skip_serializing_if = "Option::is_none")]
        xhttp_opts: Option<XHttpOpts>,
        #[serde(rename = "client-fingerprint")]
        client_fingerprint: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        flow: Option<String>,
    },
    /// mihomo/wireguard outbound. AWG uses the same shape plus
    /// `amnezia-wg-option`, which is what makes the node handshake.
    Wireguard {
        name: String,
        server: String,
        port: u16,
        /// Client address inside the tunnel (`10.x.y.z/32` style is accepted,
        /// mihomo wants a bare IP).
        ip: String,
        #[serde(rename = "private-key")]
        private_key: String,
        #[serde(rename = "public-key")]
        public_key: String,
        #[serde(rename = "allowed-ips")]
        allowed_ips: Vec<String>,
        udp: bool,
        mtu: u16,
        #[serde(
            rename = "persistent-keepalive",
            skip_serializing_if = "Option::is_none"
        )]
        persistent_keepalive: Option<u16>,
        #[serde(rename = "amnezia-wg-option", skip_serializing_if = "Option::is_none")]
        amnezia_wg_option: Option<AwgOpts>,
    },
    Hysteria2 {
        name: String,
        server: String,
        port: u16,
        password: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sni: Option<String>,
        #[serde(rename = "skip-cert-verify")]
        skip_cert_verify: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        alpn: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        obfs: Option<String>,
        #[serde(rename = "obfs-password", skip_serializing_if = "Option::is_none")]
        obfs_password: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        up: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        down: Option<String>,
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
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
}

#[derive(Serialize, Default)]
pub struct HttpHeaders {
    connection: Vec<&'static str>,
}

/// mihomo `amnezia-wg-option`: the obfuscation knobs of AmneziaWG.
#[derive(Serialize)]
pub struct AwgOpts {
    pub jc: u16,
    pub jmin: u16,
    pub jmax: u16,
    pub s1: u16,
    pub s2: u16,
    pub h1: u32,
    pub h2: u32,
    pub h3: u32,
    pub h4: u32,
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

/// Short label of a node hostname: mihomo requires unique proxy names, and a
/// pool feeds the same connection from several nodes.
fn short_host(hostname: &str) -> &str {
    hostname.split('.').next().unwrap_or(hostname)
}

pub trait InboundClashConfig {
    fn proxy(
        &self,
        conn_id: &uuid::Uuid,
        conn: &Connection,
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
                ClashProxy::Hysteria2 { name, .. } => name.clone(),
                ClashProxy::Wireguard { name, .. } => name.clone(),
            })
            .collect();

        ClashConfig {
            port: 7890,
            // `global` meant "everything goes to GLOBAL and the url-test group
            // idles"; `rule` with a MATCH tail is what mihomo users expect.
            mode: "rule",
            proxies,
            proxy_groups: vec![ClashProxyGroup {
                name: "♻️ Automatic".into(),
                group_type: "url-test".into(),
                url: "http://www.gstatic.com/generate_204".into(),
                interval: 300,
                proxies: proxy_names,
            }],
            rules: vec!["MATCH,♻️ Automatic".into()],
        }
    }

    fn proxy(
        &self,
        conn_id: &uuid::Uuid,
        conn: &Connection,
        _hostname: &str,
        host: &str,
        label: &str,
    ) -> Option<ClashProxy> {
        let port = self.port;

        // Hysteria2 lives in `h2`, not in `streamSettings`, so it is handled
        // before the xray stream checks below.
        if self.tag == Tag::Hysteria2 {
            let h2 = self.h2.as_ref()?;
            let token = conn.get_token()?;

            let name = format!(
                "{} [{}] {} {}",
                label,
                self.tag,
                get_uuid_last_octet_simple(&token),
                short_host(_hostname)
            );

            return Some(ClashProxy::Hysteria2 {
                name,
                server: h2.host.clone(),
                port,
                password: token.to_string(),
                sni: h2.sni.clone(),
                skip_cert_verify: h2.insecure,
                alpn: h2.alpn.clone(),
                obfs: h2.obfs.as_ref().map(|o| o.r#type.clone()),
                obfs_password: h2.obfs.as_ref().map(|o| o.password.clone()),
                up: h2.up_mbps.filter(|v| *v > 0).map(|v| format!("{} Mbps", v)),
                down: h2
                    .down_mbps
                    .filter(|v| *v > 0)
                    .map(|v| format!("{} Mbps", v)),
            });
        }

        // WireGuard family carries no xray stream settings: handle it before
        // the stream guard below, or every WG/AWG node is silently dropped.
        if matches!(
            self.tag,
            Tag::Wireguard | Tag::AmneziaWg | Tag::AmneziaWgMobile
        ) {
            {
                let param = conn.get_wireguard().or_else(|| conn.get_amneziawg())?;
                let (peer_pubkey, mtu, keepalive, awg_opts) = match self.tag {
                    Tag::Wireguard => {
                        let wg = self.wg.as_ref()?;
                        (
                            wg.keys.pubkey().ok()?,
                            wg.keepalive.map(|_| 1420).unwrap_or(1420),
                            wg.keepalive,
                            None,
                        )
                    }
                    _ => {
                        let awg = self.awg.as_ref()?;
                        let opts = awg.obfuscation.as_ref().and_then(|o| {
                            Some(AwgOpts {
                                jc: o.jc,
                                jmin: o.jmin,
                                jmax: o.jmax,
                                s1: o.s1,
                                s2: o.s2,
                                h1: o.h1.parse().ok()?,
                                h2: o.h2.parse().ok()?,
                                h3: o.h3.parse().ok()?,
                                h4: o.h4.parse().ok()?,
                            })
                        });
                        (
                            awg.interface.private_key.pubkey().ok()?,
                            awg.interface.mtu.unwrap_or(1420),
                            awg.keepalive,
                            opts,
                        )
                    }
                };

                // mihomo wants a bare address; the mask carries the prefix.
                let client_ip = param.address.to_string();
                let prefix = get_uuid_last_octet_simple(conn_id);
                let name = format!(
                    "{} [{}] {} {}",
                    label,
                    self.tag,
                    prefix,
                    short_host(_hostname)
                );

                return Some(ClashProxy::Wireguard {
                    name,
                    server: host.to_string(),
                    port,
                    ip: client_ip
                        .split('/')
                        .next()
                        .unwrap_or(&client_ip)
                        .to_string(),
                    private_key: param.keys.privkey.clone(),
                    public_key: peer_pubkey,
                    allowed_ips: vec!["0.0.0.0/0".to_string(), "::/0".to_string()],
                    udp: true,
                    mtu,
                    persistent_keepalive: keepalive,
                    amnezia_wg_option: awg_opts,
                });
            }
        }

        let stream = self.stream_settings.as_ref()?;

        let proxy = match self.tag {
            Tag::Vmess => {
                let tcp = stream.tcp_settings.as_ref()?;
                let header = tcp.header.as_ref()?;
                let req = header.request.as_ref()?;
                let host = req.headers.as_ref()?.get("Host")?.first()?.clone();
                let path = req.path.first().cloned().unwrap_or_else(|| "/".to_string());

                let prefix = get_uuid_last_octet_simple(conn_id);

                let name = format!(
                    "{} [{}] {} {}",
                    label,
                    self.tag,
                    prefix,
                    short_host(_hostname)
                );

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

                let (network, grpc_opts, flow, xhttp_opts) = match stream {
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
                            path: xhttp.path.clone(),
                            mode: None,
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
                let name = format!(
                    "{} [{}] {} {}",
                    label,
                    self.tag,
                    prefix,
                    short_host(_hostname)
                );

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
                    xhttp_opts,
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
                let name = format!(
                    "{} [{}] {} {}",
                    label,
                    self.tag,
                    prefix,
                    short_host(_hostname)
                );

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
                    xhttp_opts: Some(XHttpOpts {
                        path: xhttp.path.clone(),
                        mode: None,
                    }),
                    flow: None,
                })
            }

            _ => return None,
        };

        proxy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::h2::{H2Obfs, H2Settings};
    use crate::memory::connection::proto::Proto;
    use crate::memory::env::Env;

    fn h2_inbound(h2: H2Settings) -> Inbound {
        Inbound {
            tag: Tag::Hysteria2,
            port: h2.port,
            stream_settings: None,
            wg: None,
            awg: None,
            h2: Some(h2),
            mtproto_secret: None,
        }
    }

    fn h2_settings(obfs: Option<H2Obfs>, up: Option<u32>, down: Option<u32>) -> H2Settings {
        H2Settings {
            host: "hy.example.com".into(),
            port: 8443,
            sni: Some("hy.example.com".into()),
            insecure: true,
            obfs,
            alpn: Some(vec!["h3".into()]),
            up_mbps: up,
            down_mbps: down,
            auth_info: None,
        }
    }

    fn h2_conn(token: &uuid::Uuid) -> Connection {
        Connection::new(&Env::Ru, None, Proto::new_hysteria2(token), None)
    }

    #[test]
    fn hysteria2_proxy_renders_clash_verge_entry() {
        let token = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let conn_id = uuid::Uuid::parse_str("6f9619ff-8b86-d011-b42d-00c04fc964ff").unwrap();

        let proxy = h2_inbound(h2_settings(
            Some(H2Obfs {
                r#type: "salamander".into(),
                password: "obfs-pw".into(),
            }),
            Some(100),
            Some(200),
        ))
        .proxy(
            &conn_id,
            &h2_conn(&token),
            "node.example",
            "1.2.3.4",
            "Phone",
        )
        .expect("hysteria2 proxy");

        let v = serde_yaml::to_value(&proxy).unwrap();
        assert_eq!(v["type"].as_str(), Some("hysteria2"));
        assert_eq!(v["server"].as_str(), Some("hy.example.com"));
        assert_eq!(v["port"].as_u64(), Some(8443));
        assert_eq!(v["password"].as_str(), Some(token.to_string().as_str()));
        assert_eq!(v["sni"].as_str(), Some("hy.example.com"));
        assert_eq!(v["skip-cert-verify"].as_bool(), Some(true));
        assert_eq!(v["obfs"].as_str(), Some("salamander"));
        assert_eq!(v["obfs-password"].as_str(), Some("obfs-pw"));
        assert_eq!(v["up"].as_str(), Some("100 Mbps"));
        assert_eq!(v["down"].as_str(), Some("200 Mbps"));
        assert_eq!(v["alpn"][0].as_str(), Some("h3"));
        assert!(v["name"]
            .as_str()
            .unwrap()
            .starts_with("Phone [Hysteria2] "));
    }

    #[test]
    fn hysteria2_proxy_skips_optional_fields() {
        let token = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let conn_id = uuid::Uuid::parse_str("6f9619ff-8b86-d011-b42d-00c04fc964ff").unwrap();

        let mut h2 = h2_settings(None, Some(0), None);
        h2.sni = None;

        let proxy = h2_inbound(h2)
            .proxy(
                &conn_id,
                &h2_conn(&token),
                "node.example",
                "1.2.3.4",
                "Phone",
            )
            .expect("hysteria2 proxy");

        let v = serde_yaml::to_value(&proxy).unwrap();
        assert!(v.get("sni").is_none());
        assert!(v.get("obfs").is_none());
        assert!(v.get("obfs-password").is_none());
        assert!(v.get("up").is_none());
        assert!(v.get("down").is_none());
        assert_eq!(v["skip-cert-verify"].as_bool(), Some(true));
    }

    #[test]
    fn hysteria2_without_token_is_skipped() {
        let conn_id = uuid::Uuid::parse_str("6f9619ff-8b86-d011-b42d-00c04fc964ff").unwrap();
        let conn = Connection::new(&Env::Ru, None, Proto::new_xray(&Tag::VlessTcpReality), None);

        assert!(h2_inbound(h2_settings(None, None, None))
            .proxy(&conn_id, &conn, "node.example", "1.2.3.4", "Phone")
            .is_none());
    }
    fn awg_inbound() -> Inbound {
        use crate::config::amnezia_wg::{
            AmneziaWgSettings, AwgInterfaceConfig, AwgObfuscationParams,
        };
        use crate::IpAddrMask;
        use crate::WgKeys;

        Inbound {
            tag: Tag::AmneziaWgMobile,
            port: 8443,
            stream_settings: None,
            wg: None,
            awg: Some(AmneziaWgSettings {
                interface: AwgInterfaceConfig {
                    interface: "awg0".into(),
                    address: "10.8.0.1/24".parse().unwrap(),
                    listen_port: 8443,
                    mtu: Some(1280),
                    private_key: WgKeys {
                        privkey: WgKeys::default().privkey,
                    },
                    dns: vec!["1.1.1.1".parse().unwrap()],
                },
                obfuscation: Some(AwgObfuscationParams {
                    jc: 5,
                    jmin: 50,
                    jmax: 1000,
                    s1: 30,
                    s2: 40,
                    s3: 0,
                    s4: 0,
                    h1: "1000001".into(),
                    h2: "1000002".into(),
                    h3: "1000003".into(),
                    h4: "1000004".into(),
                    i1: String::new(),
                    i2: String::new(),
                    i3: String::new(),
                    i4: String::new(),
                    i5: String::new(),
                    random_trailers: None,
                    disable_cookies: None,
                    header_protection_key: None,
                    content_padding_addition: None,
                    rekey_after_time: None,
                    rekey_timeout: None,
                    reject_after_time: None,
                    keepalive_timeout: None,
                    max_handshake_attempts: None,
                }),
                keepalive: Some(25),
            }),
            h2: None,
            mtproto_secret: None,
        }
    }

    #[test]
    fn amneziawg_proxy_renders_for_mihomo() {
        let conn_id = uuid::Uuid::parse_str("6f9619ff-8b86-d011-b42d-00c04fc964ff").unwrap();
        let param = crate::WgParam::new(crate::IpAddrMask::new("10.8.0.42".parse().unwrap(), 32));
        let conn = Connection::new(&Env::Ru, None, Proto::new_awg_mobile(&param), None);

        let proxy = awg_inbound()
            .proxy(&conn_id, &conn, "node.example", "1.2.3.4", "Windows")
            .expect("awg proxy");
        let v = serde_yaml::to_value(&proxy).unwrap();

        assert_eq!(v["type"].as_str(), Some("wireguard"));
        assert_eq!(v["server"].as_str(), Some("1.2.3.4"));
        assert_eq!(v["port"].as_u64(), Some(8443));
        assert_eq!(v["mtu"].as_u64(), Some(1280));
        assert_eq!(v["ip"].as_str(), Some("10.8.0.42"));
        assert!(v["private-key"].as_str().is_some());
        assert!(v["public-key"].as_str().is_some());
        assert_eq!(v["amnezia-wg-option"]["jc"].as_u64(), Some(5));
        assert_eq!(v["amnezia-wg-option"]["h1"].as_u64(), Some(1_000_001));
        assert_eq!(v["allowed-ips"][0].as_str(), Some("0.0.0.0/0"));
    }

    #[test]
    fn clash_config_routes_with_a_rule() {
        let cfg = Inbound::clash(vec![]);
        let v = serde_yaml::to_value(&cfg).unwrap();
        assert_eq!(v["mode"].as_str(), Some("rule"));
        assert_eq!(v["rules"][0].as_str(), Some("MATCH,♻️ Automatic"));
    }

    /// Writes a representative YAML to /tmp so it can be fed to a real mihomo
    /// binary (`mihomo -t -f`). Run with `cargo test -- --ignored dump_clash`.
    #[test]
    #[ignore]
    fn dump_clash_yaml_for_mihomo_validation() {
        let conn_id = uuid::Uuid::parse_str("6f9619ff-8b86-d011-b42d-00c04fc964ff").unwrap();
        let param = crate::WgParam::new(crate::IpAddrMask::new("10.8.0.42".parse().unwrap(), 32));
        let conn = Connection::new(&Env::Ru, None, Proto::new_awg_mobile(&param), None);
        let awg = awg_inbound()
            .proxy(&conn_id, &conn, "node.example", "1.2.3.4", "Windows")
            .expect("awg proxy");
        let cfg = Inbound::clash(vec![awg]);
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        std::fs::write("/tmp/clash_sample.yaml", yaml).unwrap();
    }

    #[test]
    fn proxy_names_are_unique_across_pool_nodes() {
        // The same connection is fed from every pool node: mihomo rejects the
        // whole config when two proxies share a name.
        let conn_id = uuid::Uuid::parse_str("6f9619ff-8b86-d011-b42d-00c04fc964ff").unwrap();
        let param = crate::WgParam::new(crate::IpAddrMask::new("10.8.0.42".parse().unwrap(), 32));
        let conn = Connection::new(&Env::Ru, None, Proto::new_awg_mobile(&param), None);

        let a = awg_inbound()
            .proxy(
                &conn_id,
                &conn,
                "tw-ru-msk-core-01.example",
                "1.1.1.1",
                "RU",
            )
            .unwrap();
        let b = awg_inbound()
            .proxy(
                &conn_id,
                &conn,
                "vdska-ru-msk-core-01.example",
                "2.2.2.2",
                "RU",
            )
            .unwrap();

        let (na, nb) = match (&a, &b) {
            (ClashProxy::Wireguard { name: x, .. }, ClashProxy::Wireguard { name: y, .. }) => {
                (x.clone(), y.clone())
            }
            _ => panic!("expected wireguard proxies"),
        };
        assert_ne!(na, nb, "proxy names must differ per node");
        assert!(na.ends_with("tw-ru-msk-core-01"));
        assert!(nb.ends_with("vdska-ru-msk-core-01"));
    }
}
