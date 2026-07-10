use chrono::{DateTime, FixedOffset};
use regex::Regex;
use std::collections::HashMap;

const NGINX_TIME_FORMAT: &str = "%d/%b/%Y:%H:%M:%S %z";

#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct PixelEvent {
    pub timestamp: i64,
    pub ip: String,
    pub page: String,
    pub referer: String,
    pub referer_domain: String,
    pub user_agent: String,
    pub lang: String,
    pub utm_source: String,
    pub utm_medium: String,
    pub utm_campaign: String,
    pub utm_content: String,
    pub utm_term: String,
}

#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct ParsedLine {
    pub remote_addr: String,
    pub timestamp: i64,
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub status: u16,
    pub referer: String,
    pub user_agent: String,
}

pub struct LogParser {
    regex: Regex,
}

impl LogParser {
    pub fn new() -> Self {
        let regex = Regex::new(
            r#"^(?P<ip>\S+)\s+\S+\s+\S+\s+\[(?P<time>[^\]]+)\]\s+"(?P<method>\S+)\s+(?P<url>\S+)\s+\S+"\s+(?P<status>\d{3})\s+\S+\s+"(?P<referer>[^"]*)"\s+"(?P<ua>[^"]*)""#
        ).expect("Invalid log parser regex");
        Self { regex }
    }

    pub fn parse_line(&self, line: &str) -> Option<ParsedLine> {
        let caps = self.regex.captures(line)?;

        let remote_addr = caps.name("ip")?.as_str().to_string();
        let time_str = caps.name("time")?.as_str();
        let timestamp = parse_nginx_time(time_str).ok()?;
        let method = caps.name("method")?.as_str().to_string();
        let url = caps.name("url")?.as_str();
        let status = caps.name("status")?.as_str().parse::<u16>().ok()?;
        let referer = caps.name("referer")?.as_str().to_string();
        let user_agent = caps.name("ua")?.as_str().to_string();

        let (path, query) = parse_url(url);

        Some(ParsedLine {
            remote_addr,
            timestamp,
            method,
            path,
            query,
            status,
            referer,
            user_agent,
        })
    }

    pub fn parse_pixel_event(&self, line: &str) -> Option<PixelEvent> {
        let parsed = self.parse_line(line)?;

        if parsed.method != "GET" || !parsed.path.eq_ignore_ascii_case("/pixel") {
            return None;
        }

        let page = parsed.query.get("page").cloned().unwrap_or_default();
        let referer = parsed.query.get("ref").cloned().unwrap_or(parsed.referer.clone());
        let referer_domain = extract_domain(&referer);

        Some(PixelEvent {
            timestamp: parsed.timestamp,
            ip: parsed.remote_addr,
            page,
            referer,
            referer_domain,
            user_agent: parsed.user_agent,
            lang: parsed.query.get("lang").cloned().unwrap_or_default(),
            utm_source: parsed.query.get("utm_source").cloned().unwrap_or_default(),
            utm_medium: parsed.query.get("utm_medium").cloned().unwrap_or_default(),
            utm_campaign: parsed.query.get("utm_campaign").cloned().unwrap_or_default(),
            utm_content: parsed.query.get("utm_content").cloned().unwrap_or_default(),
            utm_term: parsed.query.get("utm_term").cloned().unwrap_or_default(),
        })
    }
}

fn parse_nginx_time(value: &str) -> Result<i64, chrono::ParseError> {
    let dt = DateTime::<FixedOffset>::parse_from_str(value, NGINX_TIME_FORMAT)?;
    Ok(dt.timestamp_millis())
}

fn parse_url(url: &str) -> (String, HashMap<String, String>) {
    let mut parts = url.splitn(2, '?');
    let path = parts.next().unwrap_or("").to_string();
    let query_str = parts.next().unwrap_or("");

    let mut query = HashMap::new();
    for pair in query_str.split('&') {
        let mut kv = pair.splitn(2, '=');
        let key = kv.next().unwrap_or("");
        let value = kv.next().unwrap_or("");
        if key.is_empty() {
            continue;
        }
        let decoded_key = urlencoding::decode(key).unwrap_or_else(|_| key.into());
        let decoded_value = urlencoding::decode(value).unwrap_or_else(|_| value.into());
        query.insert(decoded_key.into_owned(), decoded_value.into_owned());
    }

    (path, query)
}

fn extract_domain(url: &str) -> String {
    if url.is_empty() {
        return "direct".to_string();
    }

    if let Some(pos) = url.find("://") {
        let start = pos + 3;
        let remainder = &url[start..];
        let end = remainder.find('/').unwrap_or(remainder.len());
        let host = &remainder[..end];
        return host.split(':').next().unwrap_or(host).to_lowercase();
    }

    url.split('/')
        .next()
        .unwrap_or(url)
        .split(':')
        .next()
        .unwrap_or(url)
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pixel_line() {
        let line = r#"85.137.165.132 - - [10/Jul/2026:00:00:02 +0300] "GET /pixel?page=%2Fsubscription%3Fref%3Dabc&lang=ru&utm_source=telegram HTTP/1.1" 200 43 "https://frkn.org/subscription" "Mozilla/5.0""#;
        let parser = LogParser::new();
        let event = parser.parse_pixel_event(line).expect("Should parse");
        assert_eq!(event.page, "/subscription?ref=abc");
        assert_eq!(event.lang, "ru");
        assert_eq!(event.utm_source, "telegram");
        assert_eq!(event.referer_domain, "frkn.org");
    }

    #[test]
    fn test_ignore_non_pixel() {
        let line = r#"85.137.165.132 - - [10/Jul/2026:00:00:02 +0300] "POST /auth HTTP/2.0" 200 55 "-" "Go-http-client/2.0""#;
        let parser = LogParser::new();
        assert!(parser.parse_pixel_event(line).is_none());
    }

    #[test]
    fn test_extract_domain() {
        assert_eq!(extract_domain("https://frkn.org/subscription"), "frkn.org");
        assert_eq!(extract_domain("http://t.me/share"), "t.me");
        assert_eq!(extract_domain("frkn.org"), "frkn.org");
        assert_eq!(extract_domain(""), "direct");
    }
}
