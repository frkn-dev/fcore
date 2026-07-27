//! App Store IAP support: App Store Server API client + signed transaction verification.
//!
//! Uses `app-store-server-library` with a custom `Transport` backed by the reqwest 0.12
//! already used by this crate (the library's own `api-client-reqwest` feature would pull
//! a second reqwest version with a different TLS stack).

use app_store_server_library::api_client::api::app_store_server_api::AppStoreServerApiClient;
use app_store_server_library::api_client::transport::{Transport, TransportError};
use app_store_server_library::primitives::environment::Environment;
use app_store_server_library::signed_data_verifier::SignedDataVerifier;

use super::config::AppleConfig;

pub fn parse_environment(value: &str) -> Result<Environment, String> {
    match value {
        "Sandbox" | "sandbox" => Ok(Environment::Sandbox),
        "Production" | "production" => Ok(Environment::Production),
        other => Err(format!(
            "invalid apple.environment: {other} (expected \"Sandbox\" or \"Production\")"
        )),
    }
}

/// Validates the product of a verified transaction against the config.
///
/// Returns the validated product id on success.
/// An empty `allowed` list disables the restriction.
pub fn validate_product_id<'a>(
    transaction_product: Option<&'a str>,
    requested_product: Option<&str>,
    allowed: &'a [String],
) -> Result<&'a str, String> {
    let product = transaction_product
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "transaction has no product_id".to_string())?;

    if let Some(requested) = requested_product {
        if requested != product {
            return Err(format!(
                "product_id mismatch: request has \"{requested}\", transaction has \"{product}\""
            ));
        }
    }

    if !allowed.is_empty() && !allowed.iter().any(|a| a == product) {
        return Err(format!("product \"{product}\" is not allowed"));
    }

    Ok(product)
}

/// HTTP transport for the App Store Server API backed by reqwest 0.12 (rustls).
#[derive(Clone, Default)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

fn map_reqwest_error(err: reqwest::Error) -> TransportError {
    if err.is_timeout() {
        TransportError::Timeout
    } else if err.is_connect() {
        TransportError::NetworkError(format!("Connection failed: {err}"))
    } else if err.is_request() {
        TransportError::RequestFailed(format!("Request error: {err}"))
    } else {
        TransportError::Other(err.to_string())
    }
}

impl Transport for ReqwestTransport {
    async fn send(
        &self,
        req: ::http::Request<Vec<u8>>,
    ) -> Result<::http::Response<Vec<u8>>, TransportError> {
        let (parts, body) = req.into_parts();

        let mut request = self.client.request(parts.method, parts.uri.to_string());
        for (name, value) in parts.headers.iter() {
            request = request.header(name, value);
        }

        let response = request
            .body(body)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let mut builder = ::http::Response::builder().status(response.status());
        for (name, value) in response.headers().iter() {
            builder = builder.header(name, value);
        }

        let body = response.bytes().await.map_err(map_reqwest_error)?.to_vec();

        builder
            .body(body)
            .map_err(|e| TransportError::InvalidResponse(e.to_string()))
    }
}

/// App Store Server API client + JWS verifier built from the service config.
pub struct AppleIapClient {
    pub api: AppStoreServerApiClient<ReqwestTransport>,
    pub verifier: SignedDataVerifier,
    pub config: AppleConfig,
}

impl AppleIapClient {
    pub fn new(config: &AppleConfig) -> Result<Self, String> {
        let environment = parse_environment(&config.environment)?;

        if environment == Environment::Production && config.app_apple_id.is_none() {
            return Err(
                "service.apple.app_apple_id is required for the Production environment".to_string(),
            );
        }

        let signing_key = std::fs::read(&config.private_key_path).map_err(|e| {
            format!(
                "failed to read Apple private key {}: {e}",
                config.private_key_path
            )
        })?;

        let root_ca = std::fs::read(&config.root_ca_path).map_err(|e| {
            format!(
                "failed to read Apple root CA {}: {e}",
                config.root_ca_path
            )
        })?;

        let api = AppStoreServerApiClient::new(
            signing_key,
            &config.key_id,
            &config.issuer_id,
            &config.bundle_id,
            environment.clone(),
            ReqwestTransport::new(),
        )
        .map_err(|e| format!("failed to build App Store API client: {e:?}"))?;

        let verifier = SignedDataVerifier::new(
            vec![root_ca],
            environment,
            config.bundle_id.clone(),
            config.app_apple_id,
        );

        Ok(Self {
            api,
            verifier,
            config: config.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed() -> Vec<String> {
        vec![
            "frkn_premium_1_month".to_string(),
            "frkn_premium_3_month".to_string(),
            "frkn_premium_6_month".to_string(),
            "frkn_premium_12_month".to_string(),
        ]
    }

    #[test]
    fn environment_parsing() {
        assert_eq!(parse_environment("Sandbox").unwrap(), Environment::Sandbox);
        assert_eq!(parse_environment("sandbox").unwrap(), Environment::Sandbox);
        assert_eq!(
            parse_environment("Production").unwrap(),
            Environment::Production
        );
        assert!(parse_environment("Xcode").is_err());
        assert!(parse_environment("").is_err());
    }

    #[test]
    fn product_validation_table() {
        let allowed = allowed();

        // (transaction_product, requested_product, expected_ok)
        let cases: Vec<(Option<&str>, Option<&str>, bool)> = vec![
            // Happy path: allowed product, no product in the request.
            (Some("frkn_premium_1_month"), None, true),
            (Some("frkn_premium_3_month"), None, true),
            (Some("frkn_premium_6_month"), None, true),
            (Some("frkn_premium_12_month"), None, true),
            // Request product matches the transaction product.
            (
                Some("frkn_premium_1_month"),
                Some("frkn_premium_1_month"),
                true,
            ),
            // Request product mismatches the transaction product.
            (
                Some("frkn_premium_1_month"),
                Some("frkn_premium_3_month"),
                false,
            ),
            // Unknown products are rejected.
            (Some("frkn_premium_1_week"), None, false),
            (Some("com.example.other"), None, false),
            (Some(""), None, false),
            (None, None, false),
        ];

        for (tx_product, req_product, expected_ok) in cases {
            let result = validate_product_id(tx_product, req_product, &allowed);
            assert_eq!(
                result.is_ok(),
                expected_ok,
                "tx={tx_product:?} req={req_product:?}"
            );
        }

        // An empty allow-list disables the restriction.
        assert!(validate_product_id(Some("anything"), None, &[]).is_ok());
    }
}
