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

impl FromStr for Code {
    type Err = CodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let raw = s.replace("-", "");

        let bytes = base32::decode(
            base32::Alphabet::Rfc4648 { padding: false },
            &raw.to_uppercase(),
        )
        .ok_or(CodeError::InvalidFormat)?;

        if bytes.len() != 16 {
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

    pub fn parse(s: &str, secret: &[u8]) -> Option<(i16, [u8; 4])> {
        let raw = s.replace("-", "");

        let bytes = base32::decode(
            base32::Alphabet::Rfc4648 { padding: false },
            &raw.to_uppercase(),
        )?;

        if bytes.len() != 16 {
            return None;
        }

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

        Some((days, distributor))
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
}
