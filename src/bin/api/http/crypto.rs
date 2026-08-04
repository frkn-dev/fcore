use base64::Engine;
use openssl::pkey::{PKey, Private};
use openssl::rsa::Padding;
use openssl::symm::{decrypt as aes_decrypt, encrypt as aes_encrypt, Cipher};
use std::sync::Arc;
use warp::{Filter, Rejection};

/// AES context obtained while decrypting the request. Used to encrypt the response.
#[derive(Debug, Clone)]
pub struct AesContext {
    pub key: Vec<u8>,
    pub iv: Vec<u8>,
}

/// AGW encryption/decryption error.
#[derive(Debug)]
pub struct AgwCryptoError(pub String);
impl warp::reject::Reject for AgwCryptoError {}

/// Decrypts an encrypted request. If the body is plain JSON, returns it as-is
/// with an empty AES context.
pub fn decrypt_request(
    private_key: &PKey<Private>,
    body: serde_json::Value,
) -> Result<(serde_json::Value, Option<AesContext>), AgwCryptoError> {
    let key_payload = body
        .get("keyPayload")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let api_payload = body
        .get("apiPayload")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let key_payload = key_payload
        .ok_or_else(|| AgwCryptoError("keyPayload missing".to_string()))?;
    let api_payload = api_payload
        .ok_or_else(|| AgwCryptoError("apiPayload missing".to_string()))?;

    let encrypted_key = base64::engine::general_purpose::STANDARD
        .decode(key_payload)
        .map_err(|e| AgwCryptoError(format!("keyPayload base64 decode failed: {e}")))?;

    let rsa = private_key.rsa().map_err(|e| AgwCryptoError(e.to_string()))?;
    let rsa_size = rsa.size() as usize;
    let mut decrypted_key_buf = vec![0u8; rsa_size];
    let decrypted_len = rsa
        .private_decrypt(&encrypted_key, &mut decrypted_key_buf, Padding::PKCS1)
        .map_err(|e| AgwCryptoError(format!("RSA decryption failed: {e}")))?;
    decrypted_key_buf.truncate(decrypted_len);

    let key_data: serde_json::Value = serde_json::from_slice(&decrypted_key_buf)
        .map_err(|e| AgwCryptoError(format!("keyPayload JSON parse failed: {e}")))?;

    let aes_key_b64 = key_data
        .get("aes_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AgwCryptoError("aes_key missing".to_string()))?;
    let aes_iv_b64 = key_data
        .get("aes_iv")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AgwCryptoError("aes_iv missing".to_string()))?;
    // aes_salt is sent by the client but is not used for key derivation.

    let aes_key = base64::engine::general_purpose::STANDARD
        .decode(aes_key_b64)
        .map_err(|e| AgwCryptoError(format!("aes_key base64 decode failed: {e}")))?;
    if aes_key.len() != 32 {
        return Err(AgwCryptoError(format!(
            "aes_key must be 32 bytes, got {}",
            aes_key.len()
        )));
    }

    let aes_iv = base64::engine::general_purpose::STANDARD
        .decode(aes_iv_b64)
        .map_err(|e| AgwCryptoError(format!("aes_iv base64 decode failed: {e}")))?;
    // The client sends 32 bytes of IV, but AES-256-CBC only uses the first 16.
    let aes_iv: Vec<u8> = aes_iv.into_iter().take(16).collect();
    if aes_iv.len() != 16 {
        return Err(AgwCryptoError(format!(
            "aes_iv must be at least 16 bytes, got {}",
            aes_iv.len()
        )));
    }

    let encrypted_api = base64::engine::general_purpose::STANDARD
        .decode(api_payload)
        .map_err(|e| AgwCryptoError(format!("apiPayload base64 decode failed: {e}")))?;

    let decrypted_api = aes_decrypt(Cipher::aes_256_cbc(), &aes_key, Some(&aes_iv), &encrypted_api)
        .map_err(|e| AgwCryptoError(format!("AES decryption failed: {e}")))?;

    let api_json: serde_json::Value = serde_json::from_slice(&decrypted_api)
        .map_err(|e| AgwCryptoError(format!("apiPayload JSON parse failed: {e}")))?;

    Ok((
        api_json,
        Some(AesContext {
            key: aes_key,
            iv: aes_iv,
        }),
    ))
}

/// Encrypts the response with the same AES-256-CBC using PKCS#7 padding.
pub fn encrypt_response(ctx: &AesContext, plaintext: &[u8]) -> Result<Vec<u8>, AgwCryptoError> {
    aes_encrypt(Cipher::aes_256_cbc(), &ctx.key, Some(&ctx.iv), plaintext)
        .map_err(|e| AgwCryptoError(format!("AES encryption failed: {e}")))
}

/// Warp filter: automatically detects an encrypted body and decrypts it.
/// Returns an error if the body is encrypted but no private key is configured.
pub fn with_agw_decryption<T>(
    private_key: Option<Arc<PKey<Private>>>,
) -> impl Filter<Extract = (T, Option<AesContext>), Error = Rejection> + Clone
where
    T: serde::de::DeserializeOwned + Send + 'static,
{
    warp::body::json::<serde_json::Value>()
        .and_then(move |body: serde_json::Value| {
            let key = private_key.clone();
            async move {
                let key = key.ok_or_else(|| {
                    warp::reject::custom(AgwCryptoError(
                        "AGW private key is not configured".to_string(),
                    ))
                })?;

                let (decrypted, ctx) = decrypt_request(&key, body).map_err(warp::reject::custom)?;

                let typed: T = serde_json::from_value(decrypted)
                    .map_err(|e| warp::reject::custom(AgwCryptoError(format!("JSON parse failed: {e}"))))?;

                Ok::<_, Rejection>((typed, ctx))
            }
        })
        .untuple_one()
}

/// Encrypts the outgoing response if an AES context exists. Otherwise returns the response as-is.
/// The original status code is preserved (e.g. 402 for expired subscriptions).
pub async fn encrypt_gateway_reply(
    response: warp::reply::Response,
    aes_ctx: Option<AesContext>,
) -> Result<warp::reply::Response, Rejection> {
    match aes_ctx {
        Some(ctx) => {
            let (parts, body) = response.into_parts();
            let body_bytes = warp::hyper::body::to_bytes(body)
                .await
                .map_err(|e| warp::reject::custom(AgwCryptoError(format!("Failed to read response body: {e}"))))?;

            let encrypted = encrypt_response(&ctx, &body_bytes)?;

            let response = warp::http::Response::builder()
                .status(parts.status)
                .header(
                    warp::http::header::CONTENT_TYPE,
                    warp::http::HeaderValue::from_static("application/octet-stream"),
                )
                .body(warp::hyper::Body::from(encrypted))
                .map_err(|e| warp::reject::custom(AgwCryptoError(format!("Failed to build response: {e}"))))?;

            Ok(response)
        }
        None => Ok(response),
    }
}
