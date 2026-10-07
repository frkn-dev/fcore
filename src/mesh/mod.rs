//! Mesh — relay for the last-mile E2E messenger.
//!
//! The server is a blind relay: it stores only opaque ciphertext slots in
//! memory (no history tables, no plaintext anywhere) and a persistent
//! registry mapping short numeric UINs (ICQ-style) to device public keys.
//! The registry (UIN → keys, created_at, last_seen) survives restarts via
//! an rkyv snapshot written by the api service every minute and on
//! shutdown; the inbox never leaves RAM.

use chrono::Utc;
use dashmap::DashMap;
use rand::Rng;
use rkyv::Deserialize;
use std::path::Path;

use crate::error::{Error, Result};

/// TTL of a sealed inbox slot.
pub const SLOT_TTL_SECS: i64 = 7 * 24 * 3600;
/// Hard cap of pending slots per recipient.
pub const MAX_SLOTS_PER_USER: usize = 100;
/// last_seen freshness window for the lookup `online` flag.
pub const ONLINE_WINDOW_SECS: i64 = 300;
/// Allowed clock skew for signed send/pull timestamps.
pub const TS_SKEW_SECS: i64 = 300;

#[derive(Debug, Clone)]
pub struct MeshIdentity {
    pub uin: String,
    /// Hex-encoded 32-byte Noise (X25519) public key.
    pub noise_pubkey: String,
    /// Hex-encoded 32-byte Ed25519 public key.
    pub signing_pubkey: String,
    pub created_at: i64,
    pub last_seen: i64,
}

#[derive(Debug, Clone)]
pub struct MeshSlot {
    pub from_uin: String,
    /// Base64 ciphertext, opaque to the server. Never logged.
    pub ciphertext: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshError {
    BadInput(String),
    UnknownUin,
    BadSignature,
    BadTimestamp,
    InboxFull,
}

/// Snapshot payload — registry only, the inbox is memory-only by design.
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct MeshRegistrySnapshot {
    pub version: u32,
    pub identities: Vec<MeshIdentitySnapshot>,
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Clone)]
pub struct MeshIdentitySnapshot {
    pub uin: String,
    pub noise_pubkey: String,
    pub signing_pubkey: String,
    pub created_at: i64,
    pub last_seen: i64,
}

pub struct MeshRegistry {
    by_uin: DashMap<String, MeshIdentity>,
    /// noise_pubkey (hex) → uin, makes register idempotent per device key.
    by_noise: DashMap<String, String>,
    /// uin → pending sealed slots.
    inbox: DashMap<String, Vec<MeshSlot>>,
}

impl Default for MeshRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MeshRegistry {
    pub fn new() -> Self {
        Self {
            by_uin: DashMap::new(),
            by_noise: DashMap::new(),
            inbox: DashMap::new(),
        }
    }

    /// Idempotent by noise_pubkey: re-registering the same device key
    /// returns the same UIN (and refreshes last_seen).
    pub fn register(&self, noise_pubkey: &str, signing_pubkey: &str) -> MeshIdentity {
        if let Some(existing) = self.by_noise.get(noise_pubkey) {
            let uin = existing.clone();
            drop(existing);
            if let Some(mut entry) = self.by_uin.get_mut(&uin) {
                entry.last_seen = Utc::now().timestamp();
                return entry.clone();
            }
        }

        let now = Utc::now().timestamp();
        let identity = MeshIdentity {
            uin: self.generate_uin(),
            noise_pubkey: noise_pubkey.to_string(),
            signing_pubkey: signing_pubkey.to_string(),
            created_at: now,
            last_seen: now,
        };
        self.by_uin.insert(identity.uin.clone(), identity.clone());
        self.by_noise
            .insert(noise_pubkey.to_string(), identity.uin.clone());
        identity
    }

    /// 8–9 digits, never starting with 0, unique within the registry.
    fn generate_uin(&self) -> String {
        let mut rng = rand::thread_rng();
        loop {
            let n: u32 = rng.gen_range(10_000_000..=999_999_999);
            let uin = n.to_string();
            if !self.by_uin.contains_key(&uin) {
                return uin;
            }
        }
    }

    pub fn lookup(&self, uin: &str) -> Option<MeshIdentity> {
        self.by_uin.get(uin).map(|e| e.clone())
    }

    pub fn is_online(identity: &MeshIdentity) -> bool {
        Utc::now().timestamp() - identity.last_seen <= ONLINE_WINDOW_SECS
    }

    pub fn touch(&self, uin: &str) {
        if let Some(mut entry) = self.by_uin.get_mut(uin) {
            entry.last_seen = Utc::now().timestamp();
        }
    }

    /// Stores a sealed slot for `to_uin` after verifying the sender's
    /// Ed25519 signature over "send\n{to_uin}\n{ts}\n{ciphertext}".
    pub fn send(
        &self,
        from_uin: &str,
        to_uin: &str,
        ciphertext: &str,
        ts: i64,
        sig_b64: &str,
    ) -> std::result::Result<(), MeshError> {
        let now = Utc::now().timestamp();
        check_ts(ts, now)?;

        let sender = self.by_uin.get(from_uin).ok_or(MeshError::UnknownUin)?;
        let signing_pubkey = sender.signing_pubkey.clone();
        drop(sender);
        if !self.by_uin.contains_key(to_uin) {
            return Err(MeshError::UnknownUin);
        }

        let msg = format!("send\n{}\n{}\n{}", to_uin, ts, ciphertext);
        verify_ed25519(&signing_pubkey, &msg, sig_b64)?;

        let mut entry = self.inbox.entry(to_uin.to_string()).or_default();
        entry.retain(|s| s.expires_at > now);
        if entry.len() >= MAX_SLOTS_PER_USER {
            return Err(MeshError::InboxFull);
        }
        entry.push(MeshSlot {
            from_uin: from_uin.to_string(),
            ciphertext: ciphertext.to_string(),
            expires_at: now + SLOT_TTL_SECS,
        });
        drop(entry);

        self.touch(from_uin);
        tracing::debug!(
            "mesh: slot stored from {} to {} ({} bytes)",
            from_uin,
            to_uin,
            ciphertext.len()
        );
        Ok(())
    }

    /// Drains the recipient's inbox: returns the pending slots and deletes
    /// them at once. Refreshes the recipient's last_seen.
    pub fn pull(
        &self,
        uin: &str,
        ts: i64,
        sig_b64: &str,
    ) -> std::result::Result<Vec<MeshSlot>, MeshError> {
        let now = Utc::now().timestamp();
        check_ts(ts, now)?;

        let identity = self.by_uin.get(uin).ok_or(MeshError::UnknownUin)?;
        let signing_pubkey = identity.signing_pubkey.clone();
        drop(identity);

        let msg = format!("pull\n{}\n{}", uin, ts);
        verify_ed25519(&signing_pubkey, &msg, sig_b64)?;

        let slots = match self.inbox.remove(uin) {
            Some((_, slots)) => slots
                .into_iter()
                .filter(|s| s.expires_at > now)
                .collect(),
            None => Vec::new(),
        };

        self.touch(uin);
        tracing::debug!("mesh: {} pulled {} slot(s)", uin, slots.len());
        Ok(slots)
    }

    /// Drops expired slots (the periodic api snapshot task calls this).
    pub fn gc(&self) {
        let now = Utc::now().timestamp();
        self.inbox.retain(|_, slots| {
            slots.retain(|s| s.expires_at > now);
            !slots.is_empty()
        });
    }

    pub async fn save_snapshot<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let identities: Vec<MeshIdentitySnapshot> = self
            .by_uin
            .iter()
            .map(|e| {
                let id = e.value();
                MeshIdentitySnapshot {
                    uin: id.uin.clone(),
                    noise_pubkey: id.noise_pubkey.clone(),
                    signing_pubkey: id.signing_pubkey.clone(),
                    created_at: id.created_at,
                    last_seen: id.last_seen,
                }
            })
            .collect();

        let snapshot = MeshRegistrySnapshot {
            version: 1,
            identities,
        };
        let bytes = rkyv::to_bytes::<_, 4096>(&snapshot)?;

        let path = path.as_ref();
        let tmp = path.with_extension("tmp");
        tokio::fs::write(&tmp, bytes.as_slice()).await?;
        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    }

    /// Missing snapshot = fresh empty registry; a corrupted one is an error
    /// (the caller falls back to an empty registry with a warning).
    pub async fn load_snapshot<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::new());
        }

        let bytes = tokio::fs::read(path).await?;
        let archived = unsafe { rkyv::archived_root::<MeshRegistrySnapshot>(&bytes) };
        let snapshot: MeshRegistrySnapshot = archived
            .deserialize(&mut rkyv::Infallible)
            .map_err(|e| Error::Custom(format!("Mesh snapshot deserialize failed: {:?}", e)))?;

        let registry = Self::new();
        for id in snapshot.identities {
            registry.by_noise.insert(id.noise_pubkey.clone(), id.uin.clone());
            registry.by_uin.insert(
                id.uin.clone(),
                MeshIdentity {
                    uin: id.uin,
                    noise_pubkey: id.noise_pubkey,
                    signing_pubkey: id.signing_pubkey,
                    created_at: id.created_at,
                    last_seen: id.last_seen,
                },
            );
        }
        Ok(registry)
    }
}

fn check_ts(ts: i64, now: i64) -> std::result::Result<(), MeshError> {
    if (now - ts).abs() > TS_SKEW_SECS {
        return Err(MeshError::BadTimestamp);
    }
    Ok(())
}

/// Validates a hex-encoded 32-byte public key, returning it normalized
/// (lowercase).
pub fn normalize_pubkey_hex(raw: &str) -> std::result::Result<String, MeshError> {
    let normalized = raw.trim().to_ascii_lowercase();
    let bytes = hex::decode(&normalized)
        .map_err(|_| MeshError::BadInput("pubkey must be hex".to_string()))?;
    if bytes.len() != 32 {
        return Err(MeshError::BadInput(
            "pubkey must be 32 bytes (64 hex chars)".to_string(),
        ));
    }
    Ok(normalized)
}

fn verify_ed25519(
    signing_pubkey_hex: &str,
    msg: &str,
    sig_b64: &str,
) -> std::result::Result<(), MeshError> {
    use base64::Engine;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    let pk_bytes = hex::decode(signing_pubkey_hex)
        .map_err(|_| MeshError::BadInput("signing_pubkey must be hex".to_string()))?;
    let pk: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| MeshError::BadInput("signing_pubkey must be 32 bytes".to_string()))?;
    let vk = VerifyingKey::from_bytes(&pk).map_err(|_| MeshError::BadSignature)?;

    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(sig_b64)
        .map_err(|_| MeshError::BadInput("sig must be base64".to_string()))?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| MeshError::BadInput("sig must be 64 bytes".to_string()))?;
    let sig = Signature::from_bytes(&sig_arr);

    vk.verify(msg.as_bytes(), &sig)
        .map_err(|_| MeshError::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};

    fn device(seed: u8) -> (SigningKey, String, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let signing_hex = hex::encode(sk.verifying_key().to_bytes());
        // Any 32 bytes stand in for the Noise key in these tests.
        let noise_hex = hex::encode([seed.wrapping_add(100); 32]);
        (sk, signing_hex, noise_hex)
    }

    fn b64_sig(sk: &SigningKey, msg: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(sk.sign(msg.as_bytes()).to_bytes())
    }

    fn now() -> i64 {
        Utc::now().timestamp()
    }

    #[test]
    fn register_is_idempotent_per_noise_key() {
        let reg = MeshRegistry::new();
        let (_, sign, noise) = device(1);

        let first = reg.register(&noise, &sign);
        let second = reg.register(&noise, &sign);
        assert_eq!(first.uin, second.uin);

        let uin: u64 = first.uin.parse().unwrap();
        assert!((10_000_000..=999_999_999).contains(&uin));

        // A different noise key gets a different UIN.
        let (_, sign2, noise2) = device(2);
        let other = reg.register(&noise2, &sign2);
        assert_ne!(first.uin, other.uin);
    }

    #[test]
    fn lookup_online_window() {
        let reg = MeshRegistry::new();
        let (_, sign, noise) = device(3);
        let id = reg.register(&noise, &sign);
        assert!(MeshRegistry::is_online(&id));

        let mut stale = id.clone();
        stale.last_seen = now() - ONLINE_WINDOW_SECS - 1;
        assert!(!MeshRegistry::is_online(&stale));

        assert!(reg.lookup("00000001").is_none());
    }

    #[test]
    fn send_pull_roundtrip_and_drain() {
        let reg = MeshRegistry::new();
        let (sk_a, sign_a, noise_a) = device(10);
        let (sk_b, sign_b, noise_b) = device(11);
        let a = reg.register(&noise_a, &sign_a);
        let b = reg.register(&noise_b, &sign_b);

        let ts = now();
        let ct = base64::engine::general_purpose::STANDARD.encode(b"opaque-ciphertext");
        let sig = b64_sig(&sk_a, &format!("send\n{}\n{}\n{}", b.uin, ts, ct));
        reg.send(&a.uin, &b.uin, &ct, ts, &sig).unwrap();

        let pull_ts = now();
        let pull_sig = b64_sig(&sk_b, &format!("pull\n{}\n{}", b.uin, pull_ts));
        let slots = reg.pull(&b.uin, pull_ts, &pull_sig).unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].from_uin, a.uin);
        assert_eq!(slots[0].ciphertext, ct);

        // Second pull is empty — slots were deleted at once.
        let pull_sig2 = b64_sig(&sk_b, &format!("pull\n{}\n{}", b.uin, now()));
        assert!(reg.pull(&b.uin, now(), &pull_sig2).unwrap().is_empty());
    }

    #[test]
    fn send_rejects_bad_signature_and_stale_ts() {
        let reg = MeshRegistry::new();
        let (sk_a, sign_a, noise_a) = device(20);
        let (_, sign_b, noise_b) = device(21);
        let a = reg.register(&noise_a, &sign_a);
        let b = reg.register(&noise_b, &sign_b);

        let ts = now();
        let sig = b64_sig(&sk_a, &format!("send\n{}\n{}\n{}", b.uin, ts, "dGVzdA=="));
        // Tampered ciphertext — signature no longer matches.
        assert_eq!(
            reg.send(&a.uin, &b.uin, "b3RoZXI=", ts, &sig).unwrap_err(),
            MeshError::BadSignature
        );
        // Stale timestamp.
        let stale = ts - TS_SKEW_SECS - 1;
        let sig_stale = b64_sig(&sk_a, &format!("send\n{}\n{}\n{}", b.uin, stale, "dGVzdA=="));
        assert_eq!(
            reg.send(&a.uin, &b.uin, "dGVzdA==", stale, &sig_stale)
                .unwrap_err(),
            MeshError::BadTimestamp
        );
        // Unknown sender / recipient.
        assert_eq!(
            reg.send("12345678", &b.uin, "dGVzdA==", ts, &sig).unwrap_err(),
            MeshError::UnknownUin
        );
        assert_eq!(
            reg.send(&a.uin, "12345678", "dGVzdA==", ts, &sig).unwrap_err(),
            MeshError::UnknownUin
        );
    }

    #[test]
    fn pull_rejects_foreign_signature() {
        let reg = MeshRegistry::new();
        let (sk_a, sign_a, noise_a) = device(30);
        let (_, sign_b, noise_b) = device(31);
        reg.register(&noise_a, &sign_a);
        let b = reg.register(&noise_b, &sign_b);

        // A signs but tries to pull B's inbox.
        let ts = now();
        let sig = b64_sig(&sk_a, &format!("pull\n{}\n{}", b.uin, ts));
        assert_eq!(
            reg.pull(&b.uin, ts, &sig).unwrap_err(),
            MeshError::BadSignature
        );
    }

    #[test]
    fn inbox_limit_and_expiry() {
        let reg = MeshRegistry::new();
        let (sk_a, sign_a, noise_a) = device(40);
        let (_, sign_b, noise_b) = device(41);
        let a = reg.register(&noise_a, &sign_a);
        let b = reg.register(&noise_b, &sign_b);

        for i in 0..MAX_SLOTS_PER_USER {
            let ts = now();
            let ct = base64::engine::general_purpose::STANDARD.encode(format!("msg-{i}"));
            let sig = b64_sig(&sk_a, &format!("send\n{}\n{}\n{}", b.uin, ts, ct));
            reg.send(&a.uin, &b.uin, &ct, ts, &sig).unwrap();
        }
        let ts = now();
        let ct = base64::engine::general_purpose::STANDARD.encode("overflow");
        let sig = b64_sig(&sk_a, &format!("send\n{}\n{}\n{}", b.uin, ts, ct));
        assert_eq!(
            reg.send(&a.uin, &b.uin, &ct, ts, &sig).unwrap_err(),
            MeshError::InboxFull
        );

        // Expired slots are dropped by gc and never delivered.
        reg.inbox.get_mut(&b.uin).unwrap()[0].expires_at = now() - 1;
        reg.gc();
        assert_eq!(reg.inbox.get(&b.uin).unwrap().len(), MAX_SLOTS_PER_USER - 1);
    }

    #[tokio::test]
    async fn snapshot_roundtrip() {
        let reg = MeshRegistry::new();
        let (_, sign, noise) = device(50);
        let id = reg.register(&noise, &sign);

        let path = std::env::temp_dir().join(format!("mesh-test-{}.bin", uuid::Uuid::new_v4()));
        reg.save_snapshot(&path).await.unwrap();

        let loaded = MeshRegistry::load_snapshot(&path).await.unwrap();
        let restored = loaded.lookup(&id.uin).unwrap();
        assert_eq!(restored.noise_pubkey, noise);
        assert_eq!(restored.signing_pubkey, sign);
        assert_eq!(restored.created_at, id.created_at);

        // Idempotency survives the restart too.
        let again = loaded.register(&noise, &sign);
        assert_eq!(again.uin, id.uin);

        std::fs::remove_file(&path).ok();

        // Missing file = empty registry.
        let empty = MeshRegistry::load_snapshot(path.with_extension("missing"))
            .await
            .unwrap();
        assert!(empty.lookup(&id.uin).is_none());
    }

    #[test]
    fn pubkey_validation() {
        assert!(normalize_pubkey_hex(&hex::encode([7u8; 32])).is_ok());
        assert!(normalize_pubkey_hex("abcd").is_err());
        assert!(normalize_pubkey_hex(&hex::encode([7u8; 16])).is_err());
        assert!(normalize_pubkey_hex("not-hex-at-all").is_err());
        // Uppercase is normalized to lowercase.
        assert_eq!(
            normalize_pubkey_hex(&hex::encode([7u8; 32]).to_uppercase()).unwrap(),
            hex::encode([7u8; 32])
        );
    }
}
