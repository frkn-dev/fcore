use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt::Formatter;

use crate::error::Error;

#[derive(Hash, Eq, Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Env {
    Production,
    Experimental,
    Dev,
    Ru,
    Wl,
    Gaming,
    Bs,
    #[serde(rename = "bs-prem")]
    BsPrem,
    /// A shard that is not built in (`standard-ru`, `super-duper-01`). Carried
    /// **verbatim**: the name in the node config, in the database and in the
    /// bus topic (`peers-update-<env>`) is the same string, with no `custom`
    /// prefix glued on.
    #[serde(untagged)]
    Named(String),
    /// User-owned env (`custom…` / `personal…`): the historical shape, where
    /// `as_str` re-applies the `custom` prefix that `FromStr` strips.
    #[serde(untagged)]
    Custom(String),
}

impl Env {
    pub fn as_bytes(&self) -> Vec<u8> {
        match self {
            Env::Experimental => b"experimental".to_vec(),
            Env::Dev => b"dev".to_vec(),
            Env::Ru => b"ru".to_vec(),
            Env::Wl => b"wl".to_vec(),
            Env::Gaming => b"gaming".to_vec(),
            Env::Bs => b"bs".to_vec(),
            Env::BsPrem => b"bs-prem".to_vec(),
            Env::Production => b"production".to_vec(),
            Env::Named(name) => name.clone().into_bytes(),
            Env::Custom(id) => format!("custom{}", id).into_bytes(),
        }
    }

    pub fn as_str(&self) -> Cow<'_, str> {
        match self {
            Env::Dev => Cow::Borrowed("dev"),
            Env::Ru => Cow::Borrowed("ru"),
            Env::Wl => Cow::Borrowed("wl"),
            Env::Gaming => Cow::Borrowed("gaming"),
            Env::Bs => Cow::Borrowed("bs"),
            Env::BsPrem => Cow::Borrowed("bs-prem"),
            Env::Experimental => Cow::Borrowed("experimental"),
            Env::Production => Cow::Borrowed("production"),
            Env::Named(name) => Cow::Borrowed(name.as_str()),
            Env::Custom(name) => Cow::Owned(format!("custom{}", name)),
        }
    }

    pub fn personal_for(subscription_id: uuid::Uuid) -> Self {
        Env::Custom(format!("personal{}", subscription_id.as_simple()))
    }

    pub fn is_personal(&self) -> bool {
        matches!(self, Env::Custom(name) if name.starts_with("personal"))
    }

    /// Premium slot env (`custompremium-…`): a dedicated node we run for the
    /// account. It is not a personal env — the user does not create nodes
    /// there — but it is still theirs, so the cabinet may read its nodes.
    pub fn is_premium(&self) -> bool {
        matches!(self, Env::Custom(name) if name.starts_with("premium"))
    }

    /// A shard that is not one of the built-in regions. It exists only because
    /// a node was registered for it, so the connection gate must not treat it
    /// like a region we operate (see `validate_env_served`).
    pub fn is_named_shard(&self) -> bool {
        matches!(self, Env::Named(_))
    }

    pub fn is_frkn_shared(&self) -> bool {
        matches!(
            self,
            Env::Production
                | Env::Experimental
                | Env::Dev
                | Env::Ru
                | Env::Wl
                | Env::Gaming
                | Env::Bs
                | Env::BsPrem
        )
    }
}

impl std::fmt::Display for Env {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Env::Dev => write!(f, "dev"),
            Env::Ru => write!(f, "ru"),
            Env::Wl => write!(f, "wl"),
            Env::Gaming => write!(f, "gaming"),
            Env::Bs => write!(f, "bs"),
            Env::BsPrem => write!(f, "bs-prem"),
            Env::Experimental => write!(f, "experimental"),
            Env::Production => write!(f, "production"),
            Env::Named(name) => write!(f, "{}", name),
            Env::Custom(name) => write!(f, "custom{}", name),
        }
    }
}

use std::str::FromStr;

impl FromStr for Env {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "experimental" | "exp" => Ok(Env::Experimental),
            "development" | "dev" => Ok(Env::Dev),
            "production" | "prod" => Ok(Env::Production),
            "ru" => Ok(Env::Ru),
            "wl" => Ok(Env::Wl),
            "gaming" => Ok(Env::Gaming),
            "bs" => Ok(Env::Bs),
            "bs-prem" | "bsprem" => Ok(Env::BsPrem),
            s if s.starts_with("custom") => Ok(Env::Custom(strip_custom_prefix(s).to_string())),
            s if s.starts_with("personal") => Ok(Env::Custom(s.to_string())),
            s if is_shard_name(s) => Ok(Env::Named(s.to_string())),
            _ => Err(Error::Custom("Wrong Env string".into())),
        }
    }
}

/// Shape of a shard name: lowercase ASCII letters, digits and dashes, up to 32
/// chars, not starting with a dash. The same string goes into the bus topic
/// (`peers-update-<env>`), so anything else would not round-trip.
pub fn is_shard_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 32 {
        return false;
    }
    let mut bytes = s.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() || b.is_ascii_digit() => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Strip every leading `custom` marker. `as_str`/`Display` re-apply exactly
/// one, so `customX`, `customcustomX` and `X` all denote the same env.
fn strip_custom_prefix(s: &str) -> &str {
    let mut name = s;
    while let Some(rest) = name.strip_prefix("custom") {
        name = rest;
        // `custom` alone is the empty custom env, not an un-stripped prefix.
        if name.is_empty() {
            break;
        }
    }
    name
}

impl<'de> Deserialize<'de> for Env {
    /// The JSON path must normalize exactly like `FromStr`: the derived
    /// `#[serde(untagged)] Custom(String)` used to keep the raw string and
    /// `as_str` then prepended `custom` again, so a request carrying
    /// `custompersonal<id>` was stored as `customcustompersonal<id>` — an env
    /// no node serves, with a connection (and hysteria token) inside it.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        let trimmed = raw.trim().to_lowercase();
        if trimmed.is_empty() {
            return Err(serde::de::Error::custom("empty env"));
        }
        // No fallback: a name we cannot place is an error at the entry point,
        // not a silent `experimental` that swallows typos.
        trimmed.parse::<Env>().map_err(serde::de::Error::custom)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn test_env_from_str() {
        assert_eq!(Env::from_str("dev").unwrap(), Env::Dev);
        assert_eq!(Env::from_str("DEVELOPMENT").unwrap(), Env::Dev);
        assert_eq!(Env::from_str("prod").unwrap(), Env::Production);
        assert_eq!(Env::from_str("experimental").unwrap(), Env::Experimental);
        assert_eq!(Env::from_str("ru").unwrap(), Env::Ru);
        assert_eq!(Env::from_str("gaming").unwrap(), Env::Gaming);
        assert_eq!(Env::from_str("GAMING").unwrap(), Env::Gaming);
        assert!(Env::Gaming.is_frkn_shared());
        assert!(!Env::Gaming.is_personal());
        assert_eq!(Env::Gaming.to_string(), "gaming");
        assert_eq!(serde_json::to_string(&Env::Gaming).unwrap(), "\"gaming\"");
        let gaming: Env = serde_json::from_str("\"gaming\"").unwrap();
        assert_eq!(gaming, Env::Gaming);
        assert_eq!("gaming".parse::<Env>().unwrap(), Env::Gaming);
        assert_eq!(Env::from_str("bs").unwrap(), Env::Bs);
        assert_eq!(Env::from_str("BS-PREM").unwrap(), Env::BsPrem);
        assert_eq!(Env::from_str("bsprem").unwrap(), Env::BsPrem);
        assert!(Env::Bs.is_frkn_shared());
        assert!(Env::BsPrem.is_frkn_shared());
        assert!(!Env::BsPrem.is_personal());
        assert_eq!(Env::BsPrem.to_string(), "bs-prem");
        assert_eq!(serde_json::to_string(&Env::BsPrem).unwrap(), "\"bs-prem\"");
        let prem: Env = serde_json::from_str("\"bs-prem\"").unwrap();
        assert_eq!(prem, Env::BsPrem);
        let topic = format!("updates-{}", Env::BsPrem);
        assert_eq!(topic, "updates-bs-prem");
    }

    #[test]
    fn test_custom_parsing() {
        assert_eq!(
            Env::from_str("custom_env_123").unwrap(),
            Env::Custom("_env_123".to_string())
        );
        assert_eq!(
            Env::from_str("custom").unwrap(),
            Env::Custom("".to_string())
        );

        // A doubled prefix must collapse: the JSON path used to keep the raw
        // string and re-apply `custom` on output, minting `customcustomX`.
        assert_eq!(
            Env::from_str("customcustompersonalabc").unwrap(),
            Env::Custom("personalabc".to_string())
        );
        assert_eq!(
            serde_json::from_str::<Env>("\"custompersonalabc\"").unwrap(),
            Env::Custom("personalabc".to_string())
        );
        assert_eq!(
            serde_json::from_str::<Env>("\"customcustompersonalabc\"")
                .unwrap()
                .to_string(),
            "custompersonalabc"
        );
        assert_eq!(
            serde_json::from_str::<Env>("\"personalabc\"").unwrap(),
            Env::Custom("personalabc".to_string())
        );

        let custom = Env::Custom("777".to_string());
        assert_eq!(custom.to_string(), "custom777");
    }

    #[test]
    fn test_serde_serialization() {
        assert_eq!(serde_json::to_string(&Env::Dev).unwrap(), "\"dev\"");

        let custom_json = serde_json::to_string(&Env::Custom("hehe".to_string())).unwrap();
        assert_eq!(custom_json, "\"hehe\"");
    }

    #[test]
    fn test_serde_deserialization() {
        let dev: Env = serde_json::from_str("\"dev\"").unwrap();
        assert_eq!(dev, Env::Dev);

        // A dash-name is a shard and comes back verbatim; an underscore is not
        // a valid shard name and must be rejected instead of silently becoming
        // a custom env (which used to mint `customsome_random_id`).
        let shard: Env = serde_json::from_str("\"standard-ru\"").unwrap();
        assert_eq!(shard, Env::Named("standard-ru".to_string()));
        assert!(serde_json::from_str::<Env>("\"some_random_id\"").is_err());
    }

    #[test]
    fn test_no_silent_fallback() {
        assert_eq!("prod".parse::<Env>().unwrap(), Env::Production);
        // A name we cannot place is an error, never `experimental`: the old
        // `From<&str>` turned typos into a different region without a word.
        assert!("garbage with spaces".parse::<Env>().is_err());
        assert!("".parse::<Env>().is_err());
        assert!("-leading-dash".parse::<Env>().is_err());
        assert!("x".repeat(33).parse::<Env>().is_err());
    }

    #[test]
    fn test_named_shard_round_trip() {
        for name in ["standard-ru", "super-duper-01", "eu-1"] {
            let env: Env = name.parse().expect("valid shard name");
            assert_eq!(env.to_string(), name);
            assert_eq!(env.as_str(), name);
            assert_eq!(serde_json::to_string(&env).unwrap(), format!("\"{name}\""));
            let back: Env = serde_json::from_str(&format!("\"{name}\"")).unwrap();
            assert_eq!(back, env);
            assert!(env.is_named_shard());
            assert!(!env.is_personal() && !env.is_premium() && !env.is_frkn_shared());
            // the bus topic must parse back to the same env
            let topic = format!("peers-update-{env}");
            assert_eq!(topic, format!("peers-update-{name}"));
            let suffix = topic.trim_start_matches("peers-update-");
            assert_eq!(suffix.parse::<Env>().unwrap(), env);
        }
        // legacy shapes are untouched: personal stays `custompersonal…`
        let personal: Env = "personal1111".parse().unwrap();
        assert_eq!(personal.to_string(), "custompersonal1111");
        assert!(personal.is_personal());
    }

    #[test]
    fn test_personal_helpers() {
        let id = uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let env = Env::personal_for(id);
        assert!(env.is_personal());
        assert!(!env.is_frkn_shared());
        assert_eq!(
            env.to_string(),
            "custompersonal11111111222233334444555555555555"
        );
        assert!(Env::from_str(&env.to_string()).unwrap().is_personal());
        let json_name = format!("personal{}", id.as_simple());
        assert_eq!(Env::from_str(&json_name).unwrap(), env);
    }
}
