use aes_gcm::{
    aead::{Aead, KeyInit, OsRng, rand_core::RngCore},
    Aes256Gcm, Nonce,
};
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use fcore::Result;

const NONCE_LEN: usize = 12;

type HmacSha256 = Hmac<Sha256>;

pub struct EmailCipher {
    cipher: Aes256Gcm,
    key: Vec<u8>,
}

impl EmailCipher {
    pub fn new(key: &[u8]) -> Self {
        let key_arr = aes_gcm::Key::<Aes256Gcm>::from_slice(key);
        Self {
            cipher: Aes256Gcm::new(key_arr),
            key: key.to_vec(),
        }
    }

    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| fcore::Error::Custom(format!("Email encryption failed: {e}")))?;
        let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);
        Ok(base64::engine::general_purpose::STANDARD.encode(combined))
    }

    #[allow(dead_code)]
    pub fn decrypt(&self, ciphertext_b64: &str) -> Result<String> {
        let combined = base64::engine::general_purpose::STANDARD
            .decode(ciphertext_b64)
            .map_err(|e| fcore::Error::Custom(format!("Base64 decode failed: {e}")))?;
        if combined.len() < NONCE_LEN {
            return Err(fcore::Error::Custom("Ciphertext too short".to_string()));
        }
        let (nonce_bytes, ciphertext) = combined.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);
        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| fcore::Error::Custom(format!("Email decryption failed: {e}")))?;
        String::from_utf8(plaintext)
            .map_err(|e| fcore::Error::Custom(format!("Invalid UTF-8 after decryption: {e}")))
    }

    pub fn hmac(&self, email: &str) -> String {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.key).unwrap();
        mac.update(email.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}
