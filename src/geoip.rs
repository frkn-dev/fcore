//! Оффлайн-обогащение IP: ASN и страна из MaxMind GeoLite2.
//!
//! Базы читаются один раз на старте (`init`) и держатся в памяти: в горячем
//! пути `/v1/report/block` никаких сетевых вызовов быть не должно — при
//! блокировках внешний HTTP-запрос к geo-сервису просто умрёт.
//!
//! Если базы не сконфигурированы или файлов нет, `lookup` возвращает `None`,
//! и телеметрия продолжает работать без ASN (вердикты тогда не срабатывают).

use std::net::IpAddr;
use std::sync::OnceLock;

use maxminddb::geoip2;

struct GeoDb {
    asn: maxminddb::Reader<Vec<u8>>,
    country: Option<maxminddb::Reader<Vec<u8>>>,
}

static DB: OnceLock<GeoDb> = OnceLock::new();

/// Результат обогащения: номер AS и ISO-код страны.
pub type GeoInfo = (Option<i64>, Option<String>);

/// Загружает базы. Повторный вызов — no-op (первая загрузка побеждает).
pub fn init(asn_path: &str, country_path: Option<&str>) -> Result<(), String> {
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
    // Проиграв гонку, второй инициализатор просто выходит.
    let _ = DB.set(GeoDb { asn, country });
    Ok(())
}

/// true, когда базы загружены.
pub fn is_ready() -> bool {
    DB.get().is_some()
}

/// ASN и страна для адреса. Для приватных и неизвестных адресов — `(None, None)`.
pub fn lookup(ip: IpAddr) -> GeoInfo {
    let Some(db) = DB.get() else {
        return (None, None);
    };

    let asn = db
        .asn
        .lookup::<geoip2::Asn>(ip)
        .ok()
        .and_then(|a| a.autonomous_system_number)
        .map(|n| n as i64);

    let country = db.country.as_ref().and_then(|reader| {
        reader
            .lookup::<geoip2::Country>(ip)
            .ok()
            .and_then(|c| c.country)
            .and_then(|c| c.iso_code)
            .map(|s| s.to_string())
    });

    (asn, country)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_db_if_present() {
        let Ok(asn_path) = std::env::var("GEOIP_ASN_DB") else {
            return;
        };
        let country = std::env::var("GEOIP_COUNTRY_DB").ok();
        init(&asn_path, country.as_deref()).expect("init geoip");
        assert!(is_ready());
        // Google DNS — AS15169/US, Яндекс — AS13238/RU.
        let (asn, cc) = lookup("8.8.8.8".parse().unwrap());
        assert!(asn.is_some(), "ASN для 8.8.8.8 должен найтись");
        if country.is_some() {
            assert_eq!(cc.as_deref(), Some("US"));
        }
        let (asn_ru, cc_ru) = lookup("5.255.255.5".parse().unwrap());
        assert!(asn_ru.is_some());
        if country.is_some() {
            assert_eq!(cc_ru.as_deref(), Some("RU"));
        }
    }

    #[test]
    fn without_db_returns_nothing() {
        // В тестах init не вызывается, база не загружена.
        if !is_ready() {
            assert_eq!(lookup("203.0.113.7".parse().unwrap()), (None, None));
        }
    }
}
