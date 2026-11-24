use crate::error::{Error, Result};

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hysteria2Settings {
    pub listen: Option<String>,
    pub acme: Option<AcmeConfig>,
    pub tls: Option<TlsConfig>,
    pub auth: Option<AuthConfig>,
    pub obfs: Option<HysteriaObfs>,
    pub masquerade: Option<Masquerade>,
    /// Explicit hostname used for client URLs when ACME is not configured.
    /// If ACME is present the first domain from acme.domains takes precedence.
    pub host: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    pub cert: String,
    pub key: String,
}

impl Hysteria2Settings {
    pub fn from_file(path: &str) -> Result<Self> {
        let mut file = File::open(path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        let config: Hysteria2Settings = serde_yaml::from_str(&contents)?;
        Ok(config)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct H2AuthInfo {
    pub auth_type: String,
    pub has_password: bool,
    pub has_url: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcmeConfig {
    pub domains: Option<Vec<String>>,
    pub email: Option<String>,

    #[serde(rename = "type")]
    pub r#type: Option<String>,
    pub dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub r#type: Option<String>,
    pub password: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HysteriaSalamander {
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HysteriaObfs {
    pub r#type: Option<String>,
    /// Flat form (`obfs: { type, password }`), used by hysteria2 when the
    /// obfuscator takes no nested options. Salamander's nested form is
    /// `obfs: { type: salamander, salamander: { password } }` — see below.
    pub password: Option<String>,
    #[serde(default)]
    pub salamander: Option<HysteriaSalamander>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Masquerade {
    pub r#type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct H2Settings {
    pub host: String,
    pub port: u16,
    pub sni: Option<String>,
    pub insecure: bool,
    pub obfs: Option<H2Obfs>,
    pub alpn: Option<Vec<String>>,
    pub up_mbps: Option<u32>,
    pub down_mbps: Option<u32>,
    pub auth_info: Option<H2AuthInfo>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct H2Obfs {
    pub r#type: String,
    pub password: String,
}

impl Hysteria2Settings {
    pub fn validate(&self) -> Result<()> {
        if self.listen.is_none() {
            return Err(Error::Custom("Hysteria2: listen is required".into()));
        }

        let _auth = self
            .auth
            .as_ref()
            .ok_or_else(|| Error::Custom("Hysteria2: auth section is required".into()))?;

        let has_acme = self
            .acme
            .as_ref()
            .map(|a| a.domains.as_ref().map(|d| !d.is_empty()).unwrap_or(false))
            .unwrap_or(false);
        let has_tls = self.tls.is_some();

        if !has_acme && !has_tls {
            return Err(Error::Custom(
                "Hysteria2: either acme.domains or tls section is required".into(),
            ));
        }

        if !has_acme && self.host.is_none() {
            return Err(Error::Custom(
                "Hysteria2: host is required when acme is not used".into(),
            ));
        }

        Ok(())
    }
}

impl TryFrom<Hysteria2Settings> for H2Settings {
    type Error = Error;

    fn try_from(server: Hysteria2Settings) -> std::result::Result<H2Settings, Error> {
        let listen = server
            .listen
            .ok_or_else(|| Error::Custom("Hysteria2: listen missing".into()))?;

        let port = listen
            .split(':')
            .next_back()
            .unwrap_or("443")
            .parse::<u16>()
            .map_err(|_| Error::Custom("Hysteria2: invalid port".into()))?;

        let host = server
            .acme
            .as_ref()
            .and_then(|a| a.domains.as_ref())
            .and_then(|d| d.first())
            .cloned()
            .or(server.host)
            .ok_or_else(|| Error::Custom("Hysteria2: acme.domains or host is required".into()))?;

        let auth_info = server.auth.map(|a| H2AuthInfo {
            auth_type: a.r#type.unwrap_or_else(|| "unknown".into()),
            has_password: a.password.is_some(),
            has_url: a.url.is_some(),
        });

        let obfs = server.obfs.map(|o| H2Obfs {
            // Salamander declares its PSK in a nested block; the flat `password`
            // is what other obfuscators use. Normalise both into one shape so
            // the card can carry `{type, password}` regardless of the file style.
            password: o
                .password
                .clone()
                .or_else(|| o.salamander.as_ref().and_then(|s| s.password.clone()))
                .unwrap_or_default(),
            r#type: o
                .r#type
                .clone()
                .or_else(|| o.salamander.as_ref().map(|_| "salamander".to_string()))
                .unwrap_or_default(),
        });

        Ok(H2Settings {
            host: host.clone(),
            port,
            sni: Some(host),
            insecure: false,
            alpn: Some(vec!["h2".into(), "http/1.1".into()]),
            obfs,
            up_mbps: None,
            down_mbps: None,
            auth_info,
        })
    }
}

#[cfg(test)]
mod obfs_tests {
    use super::*;

    fn parse(yaml: &str) -> H2Settings {
        let cfg: Hysteria2Settings = serde_yaml::from_str(yaml).expect("yaml");
        H2Settings::try_from(cfg).expect("settings")
    }

    /// Salamander keeps its PSK in a nested block: the file style hysteria2
    /// itself expects. The card must still see `{type: salamander, password}`.
    #[test]
    fn nested_salamander_password_reaches_the_announced_inbound() {
        let h2 = parse(
            r#"
host: prem-0001.beacon.frkn.org
listen: :8443
tls:
  cert: /opt/hysteria/certs/fullchain.pem
  key: /opt/hysteria/certs/privkey.pem
auth:
  type: http
  http:
    url: https://api.frkn.org/auth
obfs:
  type: salamander
  salamander:
    password: psk-from-nested-block
"#,
        );
        let obfs = h2.obfs.expect("obfs announced");
        assert_eq!(obfs.r#type, "salamander");
        assert_eq!(obfs.password, "psk-from-nested-block");
    }

    /// The flat form stays supported (no obfuscator without nested options
    /// regressed).
    #[test]
    fn flat_password_is_still_read() {
        let h2 = parse(
            r#"
host: h2.example
listen: :443
tls:
  cert: /c.pem
  key: /k.pem
auth:
  type: password
  password: secret
obfs:
  type: salamander
  password: flat-psk
"#,
        );
        let obfs = h2.obfs.expect("obfs announced");
        assert_eq!(obfs.password, "flat-psk");
        assert_eq!(obfs.r#type, "salamander");
    }

    #[test]
    fn absent_obfs_stays_absent() {
        let h2 = parse(
            r#"
host: h2.example
listen: :443
tls:
  cert: /c.pem
  key: /k.pem
auth:
  type: password
  password: secret
"#,
        );
        assert!(h2.obfs.is_none());
    }
}
