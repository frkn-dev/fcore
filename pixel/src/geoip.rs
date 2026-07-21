use maxminddb::{geoip2, MaxMindDBError, Reader};
use std::{net::IpAddr, path::Path, sync::Arc};

pub struct GeoIpResolver {
    reader: Option<Arc<Reader<Vec<u8>>>>,
}

impl GeoIpResolver {
    pub fn new(path: &Path) -> Self {
        if !path.exists() {
            tracing::warn!("GeoIP database not found at {:?}", path);
            return Self { reader: None };
        }

        match Reader::open_readfile(path) {
            Ok(reader) => Self {
                reader: Some(Arc::new(reader)),
            },
            Err(err) => {
                tracing::error!("Failed to open GeoIP database: {}", err);
                Self { reader: None }
            }
        }
    }

    pub fn resolve_country(&self,
        ip_str: &str,
    ) -> Option<String> {
        let reader = self.reader.as_ref()?;
        let ip: IpAddr = ip_str.parse().ok()?;

        match reader.lookup::<geoip2::Country>(ip) {
            Ok(country) => country
                .country
                .and_then(|c| c.iso_code)
                .map(|code| code.to_uppercase()),
            Err(MaxMindDBError::AddressNotFoundError(_)) => Some("ZZ".to_string()),
            Err(err) => {
                tracing::debug!("GeoIP lookup failed for {}: {}", ip, err);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_missing_db_returns_none() {
        let resolver = GeoIpResolver::new(Path::new("/nonexistent/db.mmdb"));
        assert!(resolver.resolve_country("8.8.8.8").is_none());
    }
}
