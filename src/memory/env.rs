use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt::Formatter;

use crate::error::Error;

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

    pub fn personal_for(subscription_id: uuid::Uuid) -> Self {
        Env::Custom(format!("personal{}", subscription_id.as_simple()))
    }

    pub fn is_personal(&self) -> bool {
        matches!(self, Env::Custom(name) if name.starts_with("personal"))
    }

    pub fn is_frkn_shared(&self) -> bool {
        matches!(
            self,
            Env::Production | Env::Experimental | Env::Dev | Env::Ru | Env::Wl
        )
    }
}

impl std::fmt::Display for Env {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
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

        let prem: Env = serde_json::from_str("\"some_random_id\"").unwrap();
        assert_eq!(prem, Env::Custom("some_random_id".to_string()));
    }

    #[test]
    fn test_from_conversions() {
        let env: Env = "prod".into();
        assert_eq!(env, Env::Production);

        let env_fail: Env = "unknown_garbage".into();
        assert_eq!(env_fail, Env::Experimental);
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
    }
}
