use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::Error;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Key {
    pub id: uuid::Uuid,
    pub code: String,
    pub days: i16,
    pub activated: bool,
    pub subscription_id: Option<uuid::Uuid>,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
    pub distributor: Distributor,
    pub kind: KeyKind,
    pub traffic_bytes: Option<i64>,
}

/// What a key grants on activation: days (standard) or traffic (lite).
#[derive(Debug, Default, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KeyKind {
    #[default]
    Standard,
    Lite,
}

impl fmt::Display for KeyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyKind::Standard => write!(f, "standard"),
            KeyKind::Lite => write!(f, "lite"),
        }
    }
}

impl FromStr for KeyKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "standard" => Ok(KeyKind::Standard),
            "lite" => Ok(KeyKind::Lite),
            _ => Err(Error::Custom("Wrong KeyKind string".into())),
        }
    }
}

impl Key {
    pub fn new(days: i16, distributor: &Distributor, secret: &[u8]) -> Key {
        let id = uuid::Uuid::new_v4();
        let now = Utc::now();
        let code = Code::new(days, distributor.as_bytes(), secret).to_string();

        Key {
            id,
            code,
            days,
            activated: false,
            subscription_id: None,
            created_at: now,
            modified_at: now,
            distributor: *distributor,
            kind: KeyKind::Standard,
            traffic_bytes: None,
        }
    }

    /// Creates a lite (traffic-only) key: the v2 code carries traffic_gib,
    /// the days column stays 0 — a lite subscription has no time expiry.
    pub fn new_lite(traffic_gib: u32, distributor: &Distributor, secret: &[u8]) -> Key {
        let id = uuid::Uuid::new_v4();
        let now = Utc::now();
        let code = Code::new_lite(traffic_gib, distributor.as_bytes(), secret).to_string();

        Key {
            id,
            code,
            days: 0,
            activated: false,
            subscription_id: None,
            created_at: now,
            modified_at: now,
            distributor: *distributor,
            kind: KeyKind::Lite,
            traffic_bytes: Some(traffic_gib as i64 * 1024 * 1024 * 1024),
        }
    }

    pub fn activate(&mut self, sub_id: &uuid::Uuid) -> uuid::Uuid {
        let now = Utc::now();
        self.activated = true;
        self.subscription_id = Some(*sub_id);
        self.modified_at = now;
        self.id
    }
}

impl From<tokio_postgres::Row> for Key {
    fn from(row: tokio_postgres::Row) -> Self {
        Self {
            id: row.get::<_, uuid::Uuid>("id"),
            code: row.get::<_, String>("code"),
            days: row.get::<_, i16>("days"),
            activated: row.get::<_, bool>("activated"),
            subscription_id: row.get::<_, Option<uuid::Uuid>>("subscription_id"),
            created_at: row.get::<_, DateTime<Utc>>("created_at"),
            modified_at: row.get::<_, DateTime<Utc>>("modified_at"),
            distributor: {
                let dist_str: String = row.get("distributor");
                Distributor::new(&dist_str)
                    .expect("distributor in DB must be exactly 4 valid chars")
            },
            kind: {
                let kind_str: String = row.get("kind");
                KeyKind::from_str(&kind_str).unwrap_or_default()
            },
            traffic_bytes: row.get::<_, Option<i64>>("traffic_bytes"),
        }
    }
}

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Deserialize, Serialize)]
pub struct Code(String);

#[derive(Debug)]
pub enum CodeError {
    InvalidFormat,
    InvalidChecksum,
}

/// Decoded payload length of a v1 ("days") code.
const CODE_V1_LEN: usize = 16;
/// Decoded payload length of a v2 ("lite") code.
const CODE_V2_LEN: usize = 19;
/// Version byte marking a v2 ("lite") code payload.
const CODE_VERSION_LITE: u8 = 0x02;

/// What is cryptographically bound into a key code.
///
/// v1 ("days") codes carry only days; v2 ("lite") codes carry only traffic.
/// The format is detected by the decoded payload length, the version byte
/// inside v2 guards against future length collisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodePayload {
    Days {
        days: i16,
        distributor: [u8; 4],
    },
    Lite {
        traffic_gib: u32,
        distributor: [u8; 4],
    },
}

impl FromStr for Code {
    type Err = CodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let raw = s.replace("-", "");

        let bytes = base32::decode(
            base32::Alphabet::Rfc4648 { padding: false },
            &raw.to_uppercase(),
        )
        .ok_or(CodeError::InvalidFormat)?;

        if bytes.len() != CODE_V1_LEN && bytes.len() != CODE_V2_LEN {
            return Err(CodeError::InvalidFormat);
        }

        Ok(Code(s.to_uppercase()))
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Code {
    pub fn new(days: i16, distributor: &[u8; 4], secret: &[u8]) -> Self {
        let mut payload = Vec::with_capacity(15);

        let random: [u8; 6] = rand::random();
        payload.extend_from_slice(&random);
        payload.extend_from_slice(&days.to_be_bytes());
        payload.extend_from_slice(distributor);

        let sig = Self::sign(&payload, secret);
        payload.extend_from_slice(&sig[..3]);

        let check = Self::checksum(&payload);
        payload.push(check);

        let encoded =
            base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &payload).to_uppercase();

        let formatted = Self::format(&encoded);

        Code(formatted)
    }

    /// Creates a v2 ("lite") code: version byte, random, distributor and
    /// traffic in GiB are bound into the signed payload. No days — a lite
    /// subscription lives while it has traffic left.
    pub fn new_lite(traffic_gib: u32, distributor: &[u8; 4], secret: &[u8]) -> Self {
        let mut payload = Vec::with_capacity(CODE_V2_LEN - 1);

        payload.push(CODE_VERSION_LITE);
        let random: [u8; 6] = rand::random();
        payload.extend_from_slice(&random);
        payload.extend_from_slice(distributor);
        payload.extend_from_slice(&traffic_gib.to_be_bytes());

        let sig = Self::sign(&payload, secret);
        payload.extend_from_slice(&sig[..3]);

        let check = Self::checksum(&payload);
        payload.push(check);

        let encoded =
            base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &payload).to_uppercase();

        let formatted = Self::format(&encoded);

        Code(formatted)
    }

    pub fn parse(s: &str, secret: &[u8]) -> Option<(i16, [u8; 4])> {
        match Self::parse_payload(s, secret)? {
            CodePayload::Days { days, distributor } => Some((days, distributor)),
            CodePayload::Lite { .. } => None,
        }
    }

    /// Parses any supported code format, detecting it by payload length.
    pub fn parse_payload(s: &str, secret: &[u8]) -> Option<CodePayload> {
        let raw = s.replace("-", "");

        let bytes = base32::decode(
            base32::Alphabet::Rfc4648 { padding: false },
            &raw.to_uppercase(),
        )?;

        match bytes.len() {
            CODE_V1_LEN => {
                let (data_with_sig, check_byte) = bytes.split_at(15);

                if Self::checksum(data_with_sig) != check_byte[0] {
                    return None;
                }

                let (data, sig) = data_with_sig.split_at(12);

                // HMAC
                if Self::sign(data, secret)[..3] != sig[..3] {
                    return None;
                }

                let days = i16::from_be_bytes(data[6..8].try_into().ok()?);
                let distributor = data[8..12].try_into().ok()?;

                Some(CodePayload::Days { days, distributor })
            }
            CODE_V2_LEN => {
                let (data_with_sig, check_byte) = bytes.split_at(18);

                if Self::checksum(data_with_sig) != check_byte[0] {
                    return None;
                }

                let (data, sig) = data_with_sig.split_at(15);

                if data[0] != CODE_VERSION_LITE {
                    return None;
                }

                // HMAC
                if Self::sign(data, secret)[..3] != sig[..3] {
                    return None;
                }

                let distributor = data[7..11].try_into().ok()?;
                let traffic_gib = u32::from_be_bytes(data[11..15].try_into().ok()?);

                Some(CodePayload::Lite {
                    traffic_gib,
                    distributor,
                })
            }
            _ => None,
        }
    }

    fn sign(data: &[u8], secret: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    fn checksum(data: &[u8]) -> u8 {
        data.iter().fold(0u8, |acc, b| acc.wrapping_add(*b))
    }

    fn format(s: &str) -> String {
        s.chars()
            .collect::<Vec<_>>()
            .chunks(5)
            .map(|c| c.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("-")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self, secret: &[u8]) -> Result<(i16, [u8; 4]), CodeError> {
        match Self::parse(&self.0, secret) {
            Some(data) => Ok(data),
            None => Err(CodeError::InvalidChecksum),
        }
    }

    pub fn validate_payload(&self, secret: &[u8]) -> Result<CodePayload, CodeError> {
        match Self::parse_payload(&self.0, secret) {
            Some(data) => Ok(data),
            None => Err(CodeError::InvalidChecksum),
        }
    }

    pub fn is_valid(&self, secret: &[u8]) -> bool {
        Self::parse(&self.0, secret).is_some()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub struct Distributor([u8; 4]);

impl Distributor {
    pub fn new(s: &str) -> Result<Self, Error> {
        let bytes = s.as_bytes();

        if bytes.len() != 4 {
            return Err(Error::Custom(
                "Distributor must be exactly 4 characters".to_string(),
            ));
        }

        if !bytes
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            return Err(Error::Custom(
                "Distributor must be uppercase ASCII letters or digits".to_string(),
            ));
        }

        Ok(Self(bytes.try_into().unwrap()))
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap()
    }

    pub fn as_bytes(&self) -> &[u8; 4] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    const SECRET: &[u8] = b"sign-token";

    fn distributor() -> Distributor {
        Distributor::new("TEST").unwrap()
    }

    #[test]
    fn test_code_generate_and_parse() {
        let code = Code::new(10, b"TEST", SECRET);
        let code_str = code.to_string();
        let parsed = Code::parse(&code_str, SECRET).expect("Code::parse should succeed");

        assert_eq!(parsed.0, 10);
        assert_eq!(&parsed.1, b"TEST");
    }

    #[test]
    fn test_code_invalid_signature() {
        let code = Code::new(10, b"TEST", SECRET);
        let mut code_str = code.to_string();

        // Flip the FIRST character to a different valid base32 char.
        // (The last char carries unused padding bits — tampering it may
        // decode to the same bytes, which made this test flaky.)
        let first = code_str.chars().next().unwrap();
        let replacement = if first == 'A' { 'B' } else { 'A' };
        code_str.replace_range(..1, &replacement.to_string());

        let parsed = Code::parse(&code_str, SECRET);

        assert!(parsed.is_none(), "tampered code must fail");
    }

    #[test]
    fn test_code_invalid_format() {
        let bad_code = "FRKN-INVALID-CODE";
        let parsed = Code::parse(bad_code, SECRET);

        assert!(parsed.is_none());
    }

    #[test]
    fn test_code_length() {
        let code = Code::new(10, b"TEST", SECRET);
        let code_str = code.to_string();

        assert!(code_str.len() != 39, "code length is wrong");
    }

    #[test]
    fn test_key_new() {
        let key = Key::new(30, &distributor(), SECRET);

        assert_eq!(key.days, 30);
        assert!(!key.activated);
        assert!(key.subscription_id.is_none());
        assert!(!key.code.is_empty());
    }

    #[test]
    fn test_key_activation() {
        let mut key = Key::new(30, &distributor(), SECRET);
        let sub_id = Uuid::new_v4();

        key.activate(&sub_id);

        assert!(key.activated);
        assert_eq!(key.subscription_id, Some(sub_id));
    }

    #[test]
    fn test_code_invalid_checksum() {
        let code = Code::new(10, b"TEST", SECRET);
        let code_str = code.to_string();

        let mut chars: Vec<char> = code_str.chars().collect();

        for c in chars.iter_mut() {
            if *c != '-' {
                *c = if *c == 'A' { 'B' } else { 'A' };
                break;
            }
        }

        let tampered: String = chars.into_iter().collect();

        let parsed = Code::parse(&tampered, SECRET);

        assert!(parsed.is_none(), "checksum must fail");
    }

    #[test]
    fn test_checksum_catches_error_before_hmac() {
        let code = Code::new(10, b"TEST", SECRET);
        let mut code_str = code.to_string();

        code_str.replace_range(6..7, "Z");

        assert!(Code::parse(&code_str, SECRET).is_none());
    }

    #[test]
    fn test_distributor_roundtrip() {
        let code = Code::new(123, b"ABCD", SECRET);
        let parsed = Code::parse(&code.to_string(), SECRET).unwrap();

        assert_eq!(parsed.0, 123);
        assert_eq!(&parsed.1, b"ABCD");
    }

    #[test]
    fn test_days_bounds() {
        let code = Code::new(i16::MAX, b"TEST", SECRET);
        let parsed = Code::parse(&code.to_string(), SECRET).unwrap();

        assert_eq!(parsed.0, i16::MAX);
    }

    #[test]
    fn test_codes_are_different() {
        let code1 = Code::new(10, b"TEST", SECRET).to_string();
        let code2 = Code::new(10, b"TEST", SECRET).to_string();

        assert_ne!(code1, code2);
    }

    #[test]
    fn test_lite_code_generate_and_parse() {
        let code = Code::new_lite(5, b"TEST", SECRET);
        let parsed = Code::parse_payload(&code.to_string(), SECRET)
            .expect("lite code should parse");

        assert_eq!(
            parsed,
            CodePayload::Lite {
                traffic_gib: 5,
                distributor: *b"TEST",
            }
        );
    }

    #[test]
    fn test_lite_code_is_not_v1() {
        let code = Code::new_lite(5, b"TEST", SECRET);

        // v1-only entry points must reject lite codes, not misparse them.
        assert!(Code::parse(&code.to_string(), SECRET).is_none());
        assert!(code.validate(SECRET).is_err());
    }

    #[test]
    fn test_v1_code_parses_as_days_payload() {
        let code = Code::new(30, b"TEST", SECRET);
        let parsed = Code::parse_payload(&code.to_string(), SECRET).unwrap();

        assert_eq!(
            parsed,
            CodePayload::Days {
                days: 30,
                distributor: *b"TEST",
            }
        );
    }

    #[test]
    fn test_lite_code_tampered_fails() {
        let code = Code::new_lite(5, b"TEST", SECRET);
        let mut code_str = code.to_string();

        let first = code_str.chars().next().unwrap();
        let replacement = if first == 'A' { 'B' } else { 'A' };
        code_str.replace_range(..1, &replacement.to_string());

        assert!(Code::parse_payload(&code_str, SECRET).is_none());
    }

    #[test]
    fn test_lite_code_bad_version_fails() {
        // Hand-build a v2-shaped payload with a wrong version byte; HMAC is
        // valid for this data, so only the version check can reject it.
        let mut payload = Vec::with_capacity(CODE_V2_LEN - 1);
        payload.push(0x03);
        payload.extend_from_slice(&[0u8; 6]);
        payload.extend_from_slice(b"TEST");
        payload.extend_from_slice(&5u32.to_be_bytes());
        let sig = {
            let mut mac = HmacSha256::new_from_slice(SECRET).unwrap();
            mac.update(&payload);
            mac.finalize().into_bytes().to_vec()
        };
        payload.extend_from_slice(&sig[..3]);
        let check = payload.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
        payload.push(check);

        let encoded =
            base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &payload).to_uppercase();

        assert!(Code::parse_payload(&encoded, SECRET).is_none());
    }

    #[test]
    fn test_lite_code_traffic_bounds() {
        let code = Code::new_lite(u32::MAX, b"TEST", SECRET);
        let parsed = Code::parse_payload(&code.to_string(), SECRET).unwrap();

        assert_eq!(
            parsed,
            CodePayload::Lite {
                traffic_gib: u32::MAX,
                distributor: *b"TEST",
            }
        );
    }

    #[test]
    fn test_lite_code_accepts_formatted_and_raw() {
        let code = Code::new_lite(5, b"TEST", SECRET);
        let formatted = code.to_string();
        let raw = formatted.replace("-", "").to_lowercase();

        assert_eq!(
            Code::parse_payload(&raw, SECRET),
            Code::parse_payload(&formatted, SECRET),
        );
    }

    #[test]
    fn test_key_new_lite() {
        let key = Key::new_lite(5, &distributor(), SECRET);

        assert_eq!(key.kind, KeyKind::Lite);
        assert_eq!(key.days, 0);
        assert_eq!(key.traffic_bytes, Some(5 * 1024 * 1024 * 1024));
        assert!(!key.activated);
        assert!(key.subscription_id.is_none());

        // The code must round-trip through the v2 payload parser with the
        // same traffic and distributor.
        let parsed = Code::parse_payload(&key.code, SECRET).expect("lite code should parse");
        assert_eq!(
            parsed,
            CodePayload::Lite {
                traffic_gib: 5,
                distributor: *b"TEST",
            }
        );
    }

    #[test]
    fn test_key_kind_string_roundtrip() {
        for (kind, s) in [(KeyKind::Standard, "standard"), (KeyKind::Lite, "lite")] {
            assert_eq!(kind.to_string(), s);
            assert_eq!(KeyKind::from_str(s).unwrap(), kind);
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{}\"", s));
            assert_eq!(serde_json::from_str::<KeyKind>(&format!("\"{}\"", s)).unwrap(), kind);
        }

        assert!(KeyKind::from_str("premium").is_err());
        assert_eq!(KeyKind::default(), KeyKind::Standard);
    }
}
