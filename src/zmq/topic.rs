use core::fmt;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

use crate::error::Error;
use crate::memory::env::Env;

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub enum Topic {
    Auth,
    Metrics,
    /// Наблюдения о входах: с какого адреса клиент подключён к ноде.
    Entries,
    Updates(Env),
    Init(uuid::Uuid),
    /// Live peer stream of one env: every node of the env applies it.
    PeersUpdates(Env),
    /// Authoritative full set for one node: apply everything, then prune.
    PeersInit(uuid::Uuid),
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auth => write!(f, "auth"),
            Self::Metrics => write!(f, "metrics",),
            Self::Entries => write!(f, "entries",),
            Self::Updates(env) => write!(f, "updates-{}", env),
            Self::Init(uuid) => write!(f, "init-{}", uuid),
            Self::PeersUpdates(env) => write!(f, "peers-update-{}", env),
            Self::PeersInit(uuid) => write!(f, "peers-init-{}", uuid),
        }
    }
}

use std::str::FromStr;

impl FromStr for Topic {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.to_lowercase();

        if s == "auth" {
            return Ok(Topic::Auth);
        }
        if s == "metrics" {
            return Ok(Topic::Metrics);
        }
        if s == "entries" {
            return Ok(Topic::Entries);
        }

        if let Some(env_str) = s.strip_prefix("updates-") {
            let env = Env::from_str(env_str)?;
            return Ok(Topic::Updates(env));
        }

        if let Some(env_str) = s.strip_prefix("peers-update-") {
            let env = Env::from_str(env_str)?;
            return Ok(Topic::PeersUpdates(env));
        }

        if let Some(uuid_str) = s.strip_prefix("peers-init-") {
            let id = uuid::Uuid::parse_str(uuid_str)
                .map_err(|_| Error::Custom("Invalid UUID in peers-init topic".into()))?;
            return Ok(Topic::PeersInit(id));
        }

        if let Some(uuid_str) = s.strip_prefix("init-") {
            let id = uuid::Uuid::parse_str(uuid_str)
                .map_err(|_| Error::Custom("Invalid UUID in topic".into()))?;
            return Ok(Topic::Init(id));
        }

        Err(Error::Custom(format!("Unknown topic string: {}", s)))
    }
}

impl TryFrom<&str> for Topic {
    type Error = Error;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl TryFrom<String> for Topic {
    type Error = Error;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Topic::try_from(s.as_str())
    }
}

impl From<uuid::Uuid> for Topic {
    fn from(u: uuid::Uuid) -> Self {
        Topic::Init(u)
    }
}

impl From<Env> for Topic {
    fn from(e: Env) -> Self {
        Topic::Updates(e)
    }
}
impl Topic {
    pub fn as_string(&self) -> String {
        match self {
            Topic::Auth => "auth".to_string(),
            Topic::Metrics => "metrics".to_string(),
            Topic::Entries => "entries".to_string(),
            Topic::Updates(s) => format!("updates-{}", s),
            Topic::Init(s) => format!("init-{}", s),
            Topic::PeersUpdates(s) => format!("peers-update-{}", s),
            Topic::PeersInit(s) => format!("peers-init-{}", s),
        }
    }

    pub fn as_str(&self) -> Cow<'_, str> {
        match self {
            Topic::Auth => Cow::Borrowed("auth"),
            Topic::Metrics => Cow::Borrowed("metrics"),
            Topic::Entries => Cow::Borrowed("entries"),
            Topic::Updates(env) => format!("updates-{}", env).into(),
            Topic::Init(uuid) => Cow::Owned(format!("init-{}", uuid)),
            Topic::PeersUpdates(env) => Cow::Owned(format!("peers-update-{}", env)),
            Topic::PeersInit(uuid) => Cow::Owned(format!("peers-init-{}", uuid)),
        }
    }
    pub fn as_bytes(&self) -> Vec<u8> {
        match self {
            Topic::Auth => b"auth".to_vec(),
            Topic::Metrics => b"metrics".to_vec(),
            Topic::Entries => b"entries".to_vec(),
            _ => self.as_str().as_bytes().to_vec(),
        }
    }
}
