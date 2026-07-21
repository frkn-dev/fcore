use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;
use warp::Filter;

pub const VERSION: &str = "0.1.0";

pub type Result<T> = anyhow::Result<T>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Custom(String),
}

pub trait Settings: Sized {
    fn read_config<T: serde::de::DeserializeOwned>(config_file: &str) -> Result<T> {
        let config_str = std::fs::read_to_string(config_file)?;
        let settings: T = toml::from_str(&config_str)?;
        Ok(settings)
    }

    fn from_file(config_file: &str) -> Self
    where
        for<'de> Self: Deserialize<'de>,
    {
        match Self::read_config(config_file) {
            Ok(settings) => settings,
            Err(e) => panic!("Failed to load settings: {}", e),
        }
    }

    fn validate(&self) -> Result<()>;
}

#[derive(Hash, Eq, Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Env {
    Production,
    #[default]
    Experimental,
    Dev,
    Ru,
    Wl,
    #[serde(untagged)]
    Custom(String),
}

#[allow(dead_code)]
impl Env {
    pub fn as_bytes(&self) -> Vec<u8> {
        match self {
            Env::Experimental => b"experimental".to_vec(),
            Env::Dev => b"dev".to_vec(),
            Env::Ru => b"ru".to_vec(),
            Env::Wl => b"wl".to_vec(),
            Env::Production => b"production".to_vec(),
            Env::Custom(id) => format!("custom{}", id).into_bytes(),
        }
    }

    pub fn as_str(&self) -> Cow<'_, str> {
        match self {
            Env::Dev => Cow::Borrowed("dev"),
            Env::Ru => Cow::Borrowed("ru"),
            Env::Wl => Cow::Borrowed("wl"),
            Env::Experimental => Cow::Borrowed("experimental"),
            Env::Production => Cow::Borrowed("production"),
            Env::Custom(name) => Cow::Owned(format!("custom{}", name)),
        }
    }
}

impl fmt::Display for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Env::Dev => write!(f, "dev"),
            Env::Ru => write!(f, "ru"),
            Env::Wl => write!(f, "wl"),
            Env::Experimental => write!(f, "experimental"),
            Env::Production => write!(f, "production"),
            Env::Custom(name) => write!(f, "custom{}", name),
        }
    }
}

impl FromStr for Env {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "experimental" | "exp" => Ok(Env::Experimental),
            "development" | "dev" => Ok(Env::Dev),
            "production" | "prod" => Ok(Env::Production),
            "ru" => Ok(Env::Ru),
            "wl" => Ok(Env::Wl),
            s if s.starts_with("custom") => {
                let name = s.strip_prefix("custom").unwrap_or(s).to_string();
                Ok(Env::Custom(name))
            }
            _ => Err(Error::Custom("Wrong Env string".into())),
        }
    }
}

impl From<&str> for Env {
    fn from(s: &str) -> Self {
        s.parse().unwrap_or(Env::Experimental)
    }
}

impl From<String> for Env {
    fn from(s: String) -> Self {
        Env::from(s.as_str())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Copy)]
pub enum Tag {
    #[serde(rename = "VlessTcpReality")]
    VlessTcpReality,
    #[serde(rename = "VlessGrpcReality")]
    VlessGrpcReality,
    #[serde(rename = "VlessXhttpReality")]
    VlessXhttpReality,
    #[serde(rename = "VlessXhttpCdn")]
    VlessXhttpCdn,
    #[serde(rename = "Vmess")]
    Vmess,
    #[serde(rename = "Shadowsocks")]
    Shadowsocks,
    #[serde(rename = "Wireguard")]
    Wireguard,
    #[serde(rename = "AmneziaWg")]
    AmneziaWg,
    #[serde(rename = "Hysteria2")]
    Hysteria2,
    #[serde(rename = "Mtproto")]
    Mtproto,
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tag::VlessTcpReality => write!(f, "VlessTcpReality"),
            Tag::VlessGrpcReality => write!(f, "VlessGrpcReality"),
            Tag::VlessXhttpReality => write!(f, "VlessXhttpReality"),
            Tag::VlessXhttpCdn => write!(f, "VlessXhttpCdn"),
            Tag::Vmess => write!(f, "Vmess"),
            Tag::Shadowsocks => write!(f, "Shadowsocks"),
            Tag::Wireguard => write!(f, "Wireguard"),
            Tag::AmneziaWg => write!(f, "AmneziaWg"),
            Tag::Hysteria2 => write!(f, "Hysteria2"),
            Tag::Mtproto => write!(f, "Mtproto"),
        }
    }
}

impl FromStr for Tag {
    type Err = ();

    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        match input {
            "VlessTcpReality" => Ok(Tag::VlessTcpReality),
            "VlessGrpcReality" => Ok(Tag::VlessGrpcReality),
            "VlessXhttpReality" => Ok(Tag::VlessXhttpReality),
            "VlessXhttpCdn" => Ok(Tag::VlessXhttpCdn),
            "Vmess" => Ok(Tag::Vmess),
            "Shadowsocks" => Ok(Tag::Shadowsocks),
            "Wireguard" => Ok(Tag::Wireguard),
            "AmneziaWg" => Ok(Tag::AmneziaWg),
            "Hysteria2" => Ok(Tag::Hysteria2),
            "Mtproto" => Ok(Tag::Mtproto),
            _ => Err(()),
        }
    }
}

pub fn level_from_settings(level: &str) -> tracing_subscriber::EnvFilter {
    let level = match level.to_lowercase().as_str() {
        "trace" => tracing::Level::TRACE,
        "debug" => tracing::Level::DEBUG,
        "warn" => tracing::Level::WARN,
        "error" => tracing::Level::ERROR,
        _ => tracing::Level::INFO,
    };
    tracing_subscriber::EnvFilter::from_default_env().add_directive(level.into())
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AuthError(pub String);

impl warp::reject::Reject for AuthError {}

pub fn auth_filter(
    token: std::sync::Arc<String>,
) -> impl warp::Filter<Extract = (), Error = warp::Rejection> + Clone {
    warp::header::<String>("authorization")
        .and_then(move |auth_header: String| {
            let token = token.clone();
            async move {
                if auth_header
                    .strip_prefix("Bearer ")
                    .is_some_and(|t| t == token.as_str())
                {
                    Ok(())
                } else {
                    Err(warp::reject::custom(AuthError("Unauthorized".to_string())))
                }
            }
        })
        .untuple_one()
}
