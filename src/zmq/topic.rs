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
    /// Live peer stream: one env, narrowed to one pool when the node has one.
    ///
    /// The pool is part of the identity of a tunnel on the node (`us` and `eu`
    /// may hold different addresses of one key), so a pool node must not hear
    /// another pool's changes. `None` is the env-wide stream a node without a
    /// pool subscribes to.
    PeersUpdates(Env, Option<String>),
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
            Self::PeersUpdates(env, pool) => write!(f, "{}", peers_topic(env, pool.as_deref())),
            Self::PeersInit(uuid) => write!(f, "peers-init-{}", uuid),
        }
    }
}

/// `peers-update-<env>` without a pool, `peers-update-<env>-<pool>` with one.
fn peers_topic(env: &Env, pool: Option<&str>) -> String {
    match pool {
        Some(pool) => format!("peers-update-{}-{}", env, pool),
        None => format!("peers-update-{}", env),
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

        if let Some(rest) = s.strip_prefix("peers-update-") {
            // `peers-update-<env>` (env-wide) or `peers-update-<env>-<pool>`.
            // Both env and pool names may contain '-' and this string is the
            // only thing a subscriber gets, so the split is a heuristic: take
            // the longest prefix that parses as an env name. A node does not
            // depend on it — it compares the whole topic string with the one it
            // subscribed to (`peer_plane::peers_topic`) — but keep the order
            // stable so the same string always decodes to the same variant.
            for (idx, _) in rest.rmatch_indices('-') {
                let (env_part, pool_part) = (&rest[..idx], &rest[idx + 1..]);
                if pool_part.is_empty() {
                    continue;
                }
                if let Ok(env) = Env::from_str(env_part) {
                    return Ok(Topic::PeersUpdates(env, Some(pool_part.to_string())));
                }
            }
            if let Ok(env) = Env::from_str(rest) {
                return Ok(Topic::PeersUpdates(env, None));
            }
            return Err(Error::Custom(format!("Unknown peers-update topic: {}", s)));
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
            Topic::PeersUpdates(env, pool) => peers_topic(env, pool.as_deref()),
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
            Topic::PeersUpdates(env, pool) => Cow::Owned(peers_topic(env, pool.as_deref())),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str) -> Env {
        Env::from_str(name).expect("valid env")
    }

    #[test]
    fn peers_topic_round_trips_with_and_without_pool() {
        let env_wide = Topic::PeersUpdates(env("production"), None);
        assert_eq!(env_wide.to_string(), "peers-update-production");
        assert_eq!(
            "peers-update-production".parse::<Topic>().unwrap(),
            env_wide
        );

        // The shard id of the peers plane is opaque (`0001`), the pool is the
        // card key (`us`): this is what both sides actually carry.
        let scoped = Topic::PeersUpdates(env("0001"), Some("us".to_string()));
        assert_eq!(scoped.to_string(), "peers-update-0001-us");
        assert_eq!("peers-update-0001-us".parse::<Topic>().unwrap(), scoped);

        // A dash inside the pool is indistinguishable from a dash inside the
        // env: the longest env prefix wins. Pinned so this stays a known,
        // documented limit instead of drifting — and harmless, because the node
        // filters on the whole topic string, never on the parsed parts.
        let dashed = Topic::PeersUpdates(env("0001"), Some("eu-west".to_string()));
        assert_eq!(dashed.to_string(), "peers-update-0001-eu-west");
        assert_eq!(
            "peers-update-0001-eu-west".parse::<Topic>().unwrap(),
            Topic::PeersUpdates(env("0001-eu"), Some("west".to_string()))
        );
    }

    #[test]
    fn node_matches_its_own_topic_string_not_the_split() {
        // "standard-01" is a legal env name and also decodes as env "standard"
        // + pool "01", so a split is not a reliable filter. The node compares
        // the whole string with what it subscribed to instead.
        let env_wide = Topic::PeersUpdates(env("standard-01"), None).to_string();
        assert_eq!(env_wide, "peers-update-standard-01");
        assert_ne!(
            env_wide.parse::<Topic>().unwrap(),
            Topic::PeersUpdates(env("standard-01"), None),
            "documents why the node must not filter on the parsed parts"
        );
    }
}
