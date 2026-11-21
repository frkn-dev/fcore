//! Public geo lists for the OpenWrt router image: `GET /router/geo/ru.zone`
//! and `GET /router/geo/ru.zone.sha256`.
//!
//! The router ships with a baked RU IPv4 zone list, which ages with the
//! firmware. These routes serve the fresh copy maintained by the daily
//! `frkn-geo-update` systemd timer (see `deploy/geo/`), so a router can
//! refresh it with a conditional GET and fall back to its baked copy when
//! offline.
//!
//! Plain text, no AGW envelope — the same shape as `/sub/<token>` and
//! `/key/validate`. No DB, no state: the file on disk is the whole truth, and
//! its sha256 is both the strong ETag and what the `.sha256` route publishes.

use std::time::SystemTime;

use fcore::http::filters::with_param_string;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use warp::http::{header, Method, StatusCode};
use warp::Filter;

/// The only file these routes read, under `service.geo_dir`.
pub(crate) const ZONE_FILE: &str = "ru.zone";

/// `sha256sum` format is `<hash>  <name>`; the name is what the router saved
/// the list as, so `sha256sum -c` works on its side.
const ZONE_FILE_LABEL: &str = "ru.zone";

/// An ipdeny RU aggregated zone has ~8.6k CIDRs. 1000 is the "clearly not
/// truncated, not an error page" floor; it is mirrored by
/// `deploy/geo/update-ru-zone.sh`, so a broken download never gets published
/// and never gets served.
pub(crate) const MIN_ZONE_LINES: usize = 1000;

/// Defensive cap: the real file is ~90 KB. Anything larger is not our list.
const MAX_ZONE_BYTES: u64 = 4 * 1024 * 1024;

/// 3600s — the timer publishes once a day, but a router that boots mid-day
/// should still get the file from the cache without a round trip.
const CACHE_CONTROL: &str = "public, max-age=3600";

/// Which of the two companion resources is being served. Both are derived
/// from the same zone file, so they share one ETag and one Last-Modified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeoAsset {
    Zone,
    Sha256,
}

struct Snapshot {
    payload: Vec<u8>,
    sha256_hex: String,
    last_modified: String,
}

enum LoadError {
    Missing,
    Invalid(String),
    Io(std::io::Error),
}

/// The two public routes, mounted by `http::routes`. Kept in one place so the
/// whole filter chain — paths, GET/HEAD, `If-None-Match` — is testable as a
/// unit instead of only through the assembled api.
pub(crate) fn routes(
    geo_dir: String,
) -> impl warp::Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
    let zone = warp::get()
        .or(warp::head())
        .unify()
        .and(warp::path("router"))
        .and(warp::path("geo"))
        .and(warp::path("ru.zone"))
        .and(warp::path::end())
        .and(warp::method())
        .and(warp::header::optional::<String>("if-none-match"))
        .and(with_param_string(geo_dir.clone()))
        .and_then(zone_handler);

    let sha256 = warp::get()
        .or(warp::head())
        .unify()
        .and(warp::path("router"))
        .and(warp::path("geo"))
        .and(warp::path("ru.zone.sha256"))
        .and(warp::path::end())
        .and(warp::method())
        .and(warp::header::optional::<String>("if-none-match"))
        .and(with_param_string(geo_dir))
        .and_then(sha256_handler);

    zone.or(sha256)
}

/// GET/HEAD `/router/geo/ru.zone` — the zone list itself.
pub async fn zone_handler(
    method: Method,
    if_none_match: Option<String>,
    geo_dir: String,
) -> Result<warp::reply::Response, warp::Rejection> {
    Ok(serve(&geo_dir, GeoAsset::Zone, &method, if_none_match.as_deref()).await)
}

/// GET/HEAD `/router/geo/ru.zone.sha256` — the checksum of the served list.
pub async fn sha256_handler(
    method: Method,
    if_none_match: Option<String>,
    geo_dir: String,
) -> Result<warp::reply::Response, warp::Rejection> {
    Ok(serve(
        &geo_dir,
        GeoAsset::Sha256,
        &method,
        if_none_match.as_deref(),
    )
    .await)
}

async fn serve(
    geo_dir: &str,
    asset: GeoAsset,
    method: &Method,
    if_none_match: Option<&str>,
) -> warp::reply::Response {
    let path = std::path::Path::new(geo_dir).join(ZONE_FILE);
    let snapshot = match read_snapshot(&path).await {
        Ok(snapshot) => snapshot,
        Err(LoadError::Missing) => return plain(StatusCode::NOT_FOUND, "ru.zone not published\n"),
        Err(LoadError::Invalid(reason)) => {
            // Never hand out a truncated/garbage list: a router that applies
            // it would split-tunnel the wrong prefixes. Served as 503 so the
            // router keeps its baked copy instead of caching nonsense.
            tracing::error!("geo: refusing to serve {} — {reason}", path.display());
            return plain(StatusCode::SERVICE_UNAVAILABLE, "geo list unavailable\n");
        }
        Err(LoadError::Io(err)) => {
            tracing::error!("geo: reading {} failed: {err}", path.display());
            return plain(StatusCode::INTERNAL_SERVER_ERROR, "geo list unavailable\n");
        }
    };

    respond(asset, snapshot, method, if_none_match)
}

fn respond(
    asset: GeoAsset,
    snapshot: Snapshot,
    method: &Method,
    if_none_match: Option<&str>,
) -> warp::reply::Response {
    // Strong validator: the bytes are immutable per sha256, and a rewrite of
    // the same content (ipdeny re-publishes an identical file now and then)
    // deliberately keeps the ETag stable so routers answer 304.
    let etag = format!("\"sha256:{}\"", snapshot.sha256_hex);
    let payload = match asset {
        GeoAsset::Zone => snapshot.payload,
        GeoAsset::Sha256 => format!("{}  {}\n", snapshot.sha256_hex, ZONE_FILE_LABEL).into_bytes(),
    };

    let builder = warp::http::Response::builder()
        .header(header::ETAG, etag.as_str())
        .header(header::LAST_MODIFIED, snapshot.last_modified.as_str())
        .header(header::CACHE_CONTROL, CACHE_CONTROL);

    if if_none_match.is_some_and(|value| if_none_match_hits(value, &etag)) {
        // RFC 9110 §15.4.5: a 304 carries no body, no Content-Length and no
        // representation metadata — only the validators and the cache policy.
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(warp::hyper::Body::empty())
            .expect("static 304 response");
    }

    // HEAD keeps the GET headers (including Content-Length) but no body, so a
    // router can poll freshness with `curl -I` cheaply.
    let content_length = payload.len().to_string();
    let body = if *method == Method::HEAD {
        warp::hyper::Body::empty()
    } else {
        warp::hyper::Body::from(payload)
    };

    warp::http::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::ETAG, etag.as_str())
        .header(header::LAST_MODIFIED, snapshot.last_modified.as_str())
        .header(header::CACHE_CONTROL, CACHE_CONTROL)
        .header(header::CONTENT_LENGTH, content_length)
        .body(body)
        .expect("static geo response")
}

async fn read_snapshot(path: &std::path::Path) -> Result<Snapshot, LoadError> {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(LoadError::Missing),
        Err(err) => return Err(LoadError::Io(err)),
    };

    // Metadata comes from the open handle: the updater replaces the file with
    // `mv` (atomic rename), so this fd keeps the old inode and the bytes, the
    // mtime and the hash always describe the same generation.
    let meta = file.metadata().await.map_err(LoadError::Io)?;
    if meta.len() > MAX_ZONE_BYTES {
        return Err(LoadError::Invalid(format!(
            "{} bytes is larger than the {} byte cap",
            meta.len(),
            MAX_ZONE_BYTES
        )));
    }
    if meta.len() == 0 {
        return Err(LoadError::Invalid("file is empty".to_string()));
    }

    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut bytes).await.map_err(LoadError::Io)?;

    let text = std::str::from_utf8(&bytes)
        .map_err(|_| LoadError::Invalid("not valid UTF-8".to_string()))?;
    let lines = validate_zone(text, MIN_ZONE_LINES).map_err(LoadError::Invalid)?;

    let last_modified = http_date(meta.modified().map_err(LoadError::Io)?);
    let sha256_hex = hex::encode(Sha256::digest(&bytes));
    tracing::debug!(
        "geo: loaded {} ({lines} CIDRs, sha256 {sha256_hex})",
        path.display()
    );

    Ok(Snapshot {
        payload: bytes,
        sha256_hex,
        last_modified,
    })
}

/// `If-None-Match` evaluation for a strong ETag. RFC 9110 §13.1.2: the
/// header may list several validators, `*` matches any current
/// representation, and comparison is weak (a `W/` prefix is ignored).
pub(crate) fn if_none_match_hits(header_value: &str, etag: &str) -> bool {
    let wanted = etag.trim_start_matches("W/");
    header_value.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || candidate.trim_start_matches("W/") == wanted
    })
}

/// Validates an ipdeny-style aggregated zone: one IPv4 CIDR per line, no
/// blanks, at least `min_lines` entries. Returns the CIDR count.
///
/// Shape only (no host-bits check): ipdeny publishes proper network
/// addresses, and being stricter here would only add a way to reject real
/// data. CRLF and trailing whitespace are tolerated — a proxy rewriting the
/// download must not look like corruption.
pub(crate) fn validate_zone(text: &str, min_lines: usize) -> Result<usize, String> {
    let mut count = 0usize;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            return Err(format!("line {} is empty", index + 1));
        }
        if !is_ipv4_cidr(line) {
            return Err(format!(
                "line {} is not an IPv4 CIDR: {:?}",
                index + 1,
                snippet(line)
            ));
        }
        count += 1;
    }

    if count < min_lines {
        return Err(format!("only {count} CIDRs, expected at least {min_lines}"));
    }

    Ok(count)
}

/// `a.b.c.d/0..32` with decimal octets <= 255 and nothing else.
pub(crate) fn is_ipv4_cidr(line: &str) -> bool {
    let Some((addr, prefix)) = line.split_once('/') else {
        return false;
    };
    match prefix.parse::<u8>() {
        Ok(prefix) if prefix <= 32 => {}
        _ => return false,
    }

    let octets: Vec<&str> = addr.split('.').collect();
    octets.len() == 4
        && octets.iter().all(|octet| {
            !octet.is_empty()
                && octet.len() <= 3
                && octet.bytes().all(|b| b.is_ascii_digit())
                && octet.parse::<u16>().is_ok_and(|value| value <= 255)
        })
}

/// IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) — the only format HTTP/1.1
/// senders must produce for Last-Modified.
fn http_date(time: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Utc> = time.into();
    dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

fn snippet(line: &str) -> String {
    line.chars().take(48).collect()
}

fn plain(status: StatusCode, body: &'static str) -> warp::reply::Response {
    warp::http::Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(warp::hyper::Body::from(body))
        .expect("static plain response")
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "9f2c0d3b7a6e5c4d3b2a190807f6e5d4c3b2a1908070605040302010abcdef12";

    fn snapshot(zone: &str) -> Snapshot {
        Snapshot {
            payload: zone.as_bytes().to_vec(),
            sha256_hex: HEX.to_string(),
            last_modified: "Sat, 21 Nov 2026 04:30:00 GMT".to_string(),
        }
    }

    fn body_len(response: &mut warp::reply::Response) -> usize {
        let taken = std::mem::replace(response.body_mut(), warp::hyper::Body::empty());
        futures::executor::block_on(warp::hyper::body::to_bytes(taken))
            .expect("body")
            .len()
    }

    /// Throwaway `geo_dir` for the route-level tests; removed on drop.
    struct TempGeo {
        dir: std::path::PathBuf,
    }

    impl TempGeo {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("frkn-geo-test-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("temp geo dir");
            Self { dir }
        }

        fn path(&self) -> String {
            self.dir.to_string_lossy().into_owned()
        }

        fn write(&self, body: &str) {
            std::fs::write(self.dir.join(ZONE_FILE), body).expect("write zone");
        }
    }

    impl Drop for TempGeo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A list that passes `MIN_ZONE_LINES`, with unique valid CIDRs.
    fn big_zone() -> String {
        (0..MIN_ZONE_LINES)
            .map(|i| format!("10.{}.{}.0/24\n", i / 256, i % 256))
            .collect()
    }

    #[tokio::test]
    async fn routes_serve_a_published_zone_over_http() {
        let geo = TempGeo::new();
        let body = big_zone();
        geo.write(&body);
        let filter = routes(geo.path());

        let response = warp::test::request()
            .path("/router/geo/ru.zone")
            .reply(&filter)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=3600"
        );
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            body.len().to_string()
        );
        assert_eq!(response.body(), body.as_bytes());
        let etag = response.headers()[header::ETAG]
            .to_str()
            .expect("etag")
            .to_string();
        let hex = hex::encode(Sha256::digest(body.as_bytes()));
        assert_eq!(etag, format!("\"sha256:{hex}\""));

        // Conditional GET with that ETag.
        let not_modified = warp::test::request()
            .path("/router/geo/ru.zone")
            .header("if-none-match", &etag)
            .reply(&filter)
            .await;
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
        assert!(not_modified.body().is_empty());
        assert!(not_modified.headers().get(header::CONTENT_LENGTH).is_none());

        // HEAD keeps the length but sends no body.
        let head = warp::test::request()
            .method("HEAD")
            .path("/router/geo/ru.zone")
            .reply(&filter)
            .await;
        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(
            head.headers()[header::CONTENT_LENGTH],
            body.len().to_string()
        );
        assert!(head.body().is_empty());

        // The companion checksum route publishes sha256sum format.
        let sha = warp::test::request()
            .path("/router/geo/ru.zone.sha256")
            .reply(&filter)
            .await;
        assert_eq!(sha.status(), StatusCode::OK);
        assert_eq!(
            String::from_utf8_lossy(sha.body()),
            format!("{hex}  ru.zone\n")
        );
    }

    #[tokio::test]
    async fn routes_404_before_the_first_publish() {
        let geo = TempGeo::new();
        let response = warp::test::request()
            .path("/router/geo/ru.zone")
            .reply(&routes(geo.path()))
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn routes_refuse_a_truncated_file() {
        let geo = TempGeo::new();
        geo.write("10.0.0.0/8\n");
        let response = warp::test::request()
            .path("/router/geo/ru.zone")
            .reply(&routes(geo.path()))
            .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn routes_reject_a_post_with_405() {
        let geo = TempGeo::new();
        geo.write(&big_zone());
        let response = warp::test::request()
            .method("POST")
            .path("/router/geo/ru.zone")
            .reply(&routes(geo.path()))
            .await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn etag_matches_exact_list_and_star() {
        let etag = "\"sha256:abc\"";
        assert!(if_none_match_hits(etag, etag));
        assert!(if_none_match_hits("  \"sha256:abc\"  ", etag));
        // A cache may send a list of validators.
        assert!(if_none_match_hits("\"other\", \"sha256:abc\"", etag));
        // `*` matches any current representation.
        assert!(if_none_match_hits("*", etag));
        // Weak comparison ignores the W/ prefix.
        assert!(if_none_match_hits("W/\"sha256:abc\"", etag));
    }

    #[test]
    fn etag_misses_other_validators() {
        let etag = "\"sha256:abc\"";
        assert!(!if_none_match_hits("\"sha256:def\"", etag));
        assert!(!if_none_match_hits("", etag));
        assert!(!if_none_match_hits("garbage", etag));
        // A prefix of the opaque tag is not a match.
        assert!(!if_none_match_hits("\"sha256:ab\"", etag));
    }

    #[test]
    fn get_sets_caching_headers_and_the_zone_body() {
        let zone = "10.0.0.0/8\n192.168.0.0/16\n";
        let response = respond(GeoAsset::Zone, snapshot(zone), &Method::GET, None);

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            response.headers()[header::ETAG],
            "\"sha256:9f2c0d3b7a6e5c4d3b2a190807f6e5d4c3b2a1908070605040302010abcdef12\""
        );
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=3600"
        );
        assert_eq!(
            response.headers()[header::LAST_MODIFIED],
            "Sat, 21 Nov 2026 04:30:00 GMT"
        );
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            zone.len().to_string()
        );

        let mut response = response;
        assert_eq!(body_len(&mut response), zone.len());
    }

    #[test]
    fn head_keeps_headers_and_drops_the_body() {
        let zone = "10.0.0.0/8\n192.168.0.0/16\n";
        let mut head = respond(GeoAsset::Zone, snapshot(zone), &Method::HEAD, None);

        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(
            head.headers()[header::CONTENT_LENGTH],
            zone.len().to_string()
        );
        assert_eq!(body_len(&mut head), 0);
    }

    #[test]
    fn matching_if_none_match_is_304_without_body_or_length() {
        let etag = format!("\"sha256:{HEX}\"");
        let mut not_modified = respond(
            GeoAsset::Zone,
            snapshot("10.0.0.0/8\n"),
            &Method::GET,
            Some(etag.as_str()),
        );

        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(not_modified.headers()[header::ETAG], etag.as_str());
        assert!(not_modified.headers().get(header::CONTENT_LENGTH).is_none());
        assert!(not_modified.headers().get(header::CONTENT_TYPE).is_none());
        assert_eq!(body_len(&mut not_modified), 0);
    }

    #[test]
    fn stale_if_none_match_serves_the_full_body() {
        let response = respond(
            GeoAsset::Zone,
            snapshot("10.0.0.0/8\n"),
            &Method::GET,
            Some("\"sha256:old\""),
        );
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn sha256_asset_uses_sha256sum_format() {
        let mut response = respond(
            GeoAsset::Sha256,
            snapshot("10.0.0.0/8\n"),
            &Method::GET,
            None,
        );

        assert_eq!(response.status(), StatusCode::OK);
        let taken = std::mem::replace(response.body_mut(), warp::hyper::Body::empty());
        let body = futures::executor::block_on(warp::hyper::body::to_bytes(taken))
            .expect("body")
            .to_vec();
        assert_eq!(
            String::from_utf8(body).expect("utf8"),
            format!("{HEX}  ru.zone\n")
        );
    }

    #[test]
    fn accepts_a_zone_with_crlf_and_a_trailing_newline() {
        let text = "10.0.0.0/8\r\n192.168.0.0/16\r\n";
        assert_eq!(validate_zone(text, 2).expect("valid"), 2);
    }

    #[test]
    fn rejects_truncated_and_malformed_lists() {
        // Too few entries = a truncated download or an error page.
        assert!(validate_zone("10.0.0.0/8\n", MIN_ZONE_LINES).is_err());
        assert!(validate_zone("10.0.0.0/8\n", 2).is_err());
        // Empty file.
        assert!(validate_zone("", 2).is_err());
        // Blank line in the middle.
        assert!(validate_zone("10.0.0.0/8\n\n192.168.0.0/16\n", 2).is_err());
    }

    #[test]
    fn rejects_malformed_cidrs() {
        for line in [
            "10.0.0.0",     // no prefix
            "10.0.0.0/33",  // prefix out of range
            "10.0.0.0/-1",  // negative prefix
            "256.0.0.0/8",  // octet out of range
            "10.0.0/8",     // three octets
            "10.0.0.0.0/8", // five octets
            "10.0.0.a/8",   // not decimal
            "10.0.0.0/08x", // not a number
            "10.0.0.0/8/9", // two prefixes
            "<html>",       // an error page
        ] {
            assert!(!is_ipv4_cidr(line.trim()), "accepted {line:?}");
        }
        // Valid edge cases.
        assert!(is_ipv4_cidr("0.0.0.0/0"));
        assert!(is_ipv4_cidr("255.255.255.255/32"));
    }

    #[test]
    fn http_date_is_imf_fixdate() {
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(784_111_777);
        assert_eq!(http_date(time), "Sun, 06 Nov 1994 08:49:37 GMT");
    }
}
