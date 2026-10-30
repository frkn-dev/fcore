//! Оффлайн-обогащение IP: ASN, оператор, страна и город из MaxMind GeoLite2.
//!
//! Базы читаются один раз на старте (`init`) и держатся в памяти: в горячем
//! пути `/v1/report/block` никаких сетевых вызовов быть не должно — при
//! блокировках внешний HTTP-запрос к geo-сервису просто умрёт.
//!
//! Если базы не сконфигурированы или файлов нет, `lookup` возвращает пустые
//! значения, и телеметрия продолжает работать без ASN (вердикты тогда не
//! срабатывают). City-база опциональна: без неё нет только города.

use std::net::IpAddr;
use std::sync::OnceLock;

use maxminddb::geoip2;

struct GeoDb {
    asn: maxminddb::Reader<Vec<u8>>,
    country: Option<maxminddb::Reader<Vec<u8>>>,
    city: Option<maxminddb::Reader<Vec<u8>>>,
}

static DB: OnceLock<GeoDb> = OnceLock::new();

/// Результат обогащения: номер AS и ISO-код страны.
pub type GeoInfo = (Option<i64>, Option<String>);

/// Полное обогащение: то, что видит поддержка в отчёте — «Мегафон · Владивосток».
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeoDetails {
    pub asn: Option<i64>,
    /// Оператор/владелец AS: «MegaFon», «MTS PJSC», «Rostelecom».
    pub org: Option<String>,
    pub country: Option<String>,
    /// Город на английском (`names.en`) — так он стабилен между базами и не
    /// зависит от локали api.
    pub city: Option<String>,
}

/// Загружает базы. Повторный вызов — no-op (первая загрузка побеждает).
pub fn init(
    asn_path: &str,
    country_path: Option<&str>,
    city_path: Option<&str>,
) -> Result<(), String> {
    if DB.get().is_some() {
        return Ok(());
    }
    let asn = maxminddb::Reader::open_readfile(asn_path)
        .map_err(|e| format!("asn db {asn_path}: {e}"))?;
    let country = match country_path {
        Some(p) if !p.is_empty() => match maxminddb::Reader::open_readfile(p) {
            Ok(r) => Some(r),
            Err(e) => return Err(format!("country db {p}: {e}")),
        },
        _ => None,
    };
    let city = match city_path {
        Some(p) if !p.is_empty() => match maxminddb::Reader::open_readfile(p) {
            Ok(r) => Some(r),
            Err(e) => return Err(format!("city db {p}: {e}")),
        },
        _ => None,
    };
    // Проиграв гонку, второй инициализатор просто выходит.
    let _ = DB.set(GeoDb {
        asn,
        country,
        city,
    });
    Ok(())
}

/// true, когда базы загружены.
pub fn is_ready() -> bool {
    DB.get().is_some()
}

/// ASN и страна для адреса. Для приватных и неизвестных адресов — `(None, None)`.
pub fn lookup(ip: IpAddr) -> GeoInfo {
    let d = lookup_full(ip);
    (d.asn, d.country)
}

/// Полное обогащение: ASN, оператор, страна, город.
pub fn lookup_full(ip: IpAddr) -> GeoDetails {
    let Some(db) = DB.get() else {
        return GeoDetails::default();
    };

    let asn_rec = db.asn.lookup::<geoip2::Asn>(ip).ok();
    let asn = asn_rec
        .as_ref()
        .and_then(|a| a.autonomous_system_number)
        .map(|n| n as i64);
    let org = asn_rec
        .as_ref()
        .and_then(|a| a.autonomous_system_organization)
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty());

    let country = db.country.as_ref().and_then(|reader| {
        reader
            .lookup::<geoip2::Country>(ip)
            .ok()
            .and_then(|c| c.country)
            .and_then(|c| c.iso_code)
            .map(|s| s.to_string())
    });

    // Город берём из City-базы, а если её нет — страну хотя бы из неё же.
    let (city, city_country) = match db.city.as_ref() {
        Some(reader) => match reader.lookup::<geoip2::City>(ip) {
            Ok(c) => {
                let city = c
                    .city
                    .and_then(|c| c.names)
                    .and_then(|n| n.get("en").copied())
                    .map(|s| s.to_string())
                    .filter(|s| !s.trim().is_empty());
                let country = c
                    .country
                    .and_then(|c| c.iso_code)
                    .map(|s| s.to_string());
                (city, country)
            }
            Err(_) => (None, None),
        },
        None => (None, None),
    };

    GeoDetails {
        asn,
        org,
        country: country.or(city_country),
        city,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_without_db_is_empty() {
        // Без инициализации (или при отсутствии файлов) ничего не падает.
        let d = lookup_full("203.0.113.1".parse().unwrap());
        assert_eq!(d.asn, None);
        assert_eq!(d.org, None);
        assert_eq!(d.city, None);
    }

    #[test]
    fn real_db_if_present() {
        let Ok(asn_path) = std::env::var("GEOIP_ASN_DB") else {
            return;
        };
        let _ = init(
            &asn_path,
            std::env::var("GEOIP_COUNTRY_DB").ok().as_deref(),
            std::env::var("GEOIP_CITY_DB").ok().as_deref(),
        );

        // Приватные адреса в базах отсутствуют.
        let private = lookup_full("10.0.0.1".parse().unwrap());
        assert_eq!(private.asn, None);
        assert_eq!(private.city, None);
    }
}
