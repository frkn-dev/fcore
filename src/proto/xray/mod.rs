pub(crate) mod client;
pub(crate) mod shadowsocks;
pub(crate) mod stats;
pub(crate) mod vless;
pub(crate) mod vmess;
pub(crate) mod trojan;

pub mod api {
    pub mod common {
        pub mod log {
            tonic::include_proto!("xray.common.log");
        }
        pub mod net {
            tonic::include_proto!("xray.common.net");
        }
        pub mod protocol {
            tonic::include_proto!("xray.common.protocol");
        }
        pub mod serial {
            tonic::include_proto!("xray.common.serial");
        }
    }
    pub mod transport {
        pub mod internet {
            tonic::include_proto!("xray.transport.internet");
        }
    }
    pub mod app {
        pub mod proxyman {
            tonic::include_proto!("xray.app.proxyman");
            pub mod command {
                tonic::include_proto!("xray.app.proxyman.command");
            }
        }
        pub mod log {
            tonic::include_proto!("xray.app.log");
            pub mod command {
                tonic::include_proto!("xray.app.log.command");
            }
        }
        pub mod stats {
            tonic::include_proto!("xray.app.stats");
            pub mod command {
                tonic::include_proto!("xray.app.stats.command");
            }
        }
    }
    pub mod core {
        tonic::include_proto!("xray.core");
    }
    pub mod proxy {
        pub mod vless {
            tonic::include_proto!("xray.proxy.vless");
            pub mod inbound {
                tonic::include_proto!("xray.proxy.vless.inbound");
            }
        }
        pub mod vmess {
            tonic::include_proto!("xray.proxy.vmess");
            pub mod inbound {
                tonic::include_proto!("xray.proxy.vmess.inbound");
            }
        }
        pub mod trojan {
            tonic::include_proto!("xray.proxy.trojan");
        }
        pub mod shadowsocks {
            tonic::include_proto!("xray.proxy.shadowsocks");
        }
    }
}
