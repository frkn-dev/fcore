use crate::parser::PixelEvent;
use crate::geoip::GeoIpResolver;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Debug)]
pub struct MetricSample {
    pub name: String,
    pub tags: BTreeMap<String, String>,
    pub value: f64,
    pub timestamp_ms: i64,
}

pub struct Aggregator {
    bucket_minutes: u64,
    buckets: HashMap<i64, Bucket>,
}

#[derive(Default)]
struct Bucket {
    metrics: HashMap<String, HashMap<String, f64>>,
}

impl Aggregator {
    pub fn new(bucket_minutes: u64) -> Self {
        Self {
            bucket_minutes,
            buckets: HashMap::new(),
        }
    }

    pub fn record(
        &mut self,
        event: PixelEvent,
        geoip: &GeoIpResolver,
    ) {
        let bucket_ms = self.bucket_time(event.timestamp);
        let bucket = self.buckets.entry(bucket_ms).or_default();

        let country = geoip.resolve_country(&event.ip).unwrap_or_else(|| "ZZ".to_string());

        bucket.increment("web.visits.total", BTreeMap::new());

        let mut page_tags = BTreeMap::new();
        page_tags.insert("page".to_string(), normalize_page(&event.page));
        bucket.increment("web.visits.page", page_tags);

        let mut host_tags = BTreeMap::new();
        host_tags.insert("host".to_string(), event.host.clone());
        bucket.increment("web.visits.host", host_tags);

        let mut country_tags = BTreeMap::new();
        country_tags.insert("country".to_string(), country.clone());
        bucket.increment("web.visits.country", country_tags);

        if !event.referer_domain.is_empty() && event.referer_domain != "direct" {
            let mut referer_tags = BTreeMap::new();
            referer_tags.insert("referer_domain".to_string(), event.referer_domain.clone());
            bucket.increment("web.visits.referer_domain", referer_tags);
        }

        if !event.lang.is_empty() {
            let mut lang_tags = BTreeMap::new();
            lang_tags.insert("lang".to_string(), event.lang.clone());
            bucket.increment("web.visits.lang", lang_tags);
        }

        for (key, value) in [
            ("utm_source", event.utm_source.as_str()),
            ("utm_medium", event.utm_medium.as_str()),
            ("utm_campaign", event.utm_campaign.as_str()),
            ("utm_content", event.utm_content.as_str()),
            ("utm_term", event.utm_term.as_str()),
        ] {
            if !value.is_empty() {
                let mut tags = BTreeMap::new();
                tags.insert(key.to_string(), value.to_string());
                bucket.increment(format!("web.visits.{}", key), tags);
            }
        }
    }

    pub fn flush(&mut self,
        retention_hours: u64,
    ) -> Vec<MetricSample> {
        let now = chrono::Utc::now().timestamp_millis();
        let retention_ms = (retention_hours * 60 * 60 * 1000) as i64;

        self.buckets.retain(|ts, _| now - ts <= retention_ms);

        let mut samples = Vec::new();
        for (&bucket_ms, bucket) in &self.buckets {
            for (name, series) in &bucket.metrics {
                for (tags_key, value) in series {
                    let tags = tags_from_key(tags_key);
                    samples.push(MetricSample {
                        name: name.clone(),
                        tags,
                        value: *value,
                        timestamp_ms: bucket_ms,
                    });
                }
            }
        }
        samples
    }

    #[allow(dead_code)]
    pub fn clear_flushed(&mut self, sent_up_to: i64) {
        self.buckets.retain(|ts, _| *ts > sent_up_to);
    }

    fn bucket_time(&self,
        timestamp_ms: i64,
    ) -> i64 {
        let interval_ms = (self.bucket_minutes * 60 * 1000) as i64;
        (timestamp_ms / interval_ms) * interval_ms
    }
}

impl Bucket {
    fn increment(&mut self,
        name: impl Into<String>,
        tags: BTreeMap<String, String>,
    ) {
        let key = tags_to_key(&tags);
        let series = self.metrics.entry(name.into()).or_default();
        *series.entry(key).or_insert(0.0) += 1.0;
    }
}

fn tags_to_key(tags: &BTreeMap<String, String>) -> String {
    let mut parts: Vec<String> = tags
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect();
    parts.sort();
    parts.join("&")
}

fn tags_from_key(key: &str) -> BTreeMap<String, String> {
    let mut tags = BTreeMap::new();
    for pair in key.split('&') {
        let mut kv = pair.splitn(2, '=');
        if let (Some(k), Some(v)) = (kv.next(), kv.next()) {
            tags.insert(k.to_string(), v.to_string());
        }
    }
    tags
}

fn normalize_page(page: &str) -> String {
    if page.is_empty() {
        return "/".to_string();
    }
    let without_hash = page.split('#').next().unwrap_or(page);
    if without_hash.is_empty() {
        return "/".to_string();
    }
    without_hash.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_aggregator() {
        let mut aggregator = Aggregator::new(5);
        let geoip = GeoIpResolver::new(Path::new("/nonexistent"));
        let now = chrono::Utc::now().timestamp_millis();

        aggregator.record(PixelEvent {
            timestamp: now,
            ip: "85.137.165.132".to_string(),
            page: "/subscription".to_string(),
            host: "hehe.frkn.org".to_string(),
            referer: "https://frkn.org".to_string(),
            referer_domain: "frkn.org".to_string(),
            user_agent: "Mozilla".to_string(),
            lang: "ru".to_string(),
            utm_source: "telegram".to_string(),
            ..Default::default()
        }, &geoip);

        let samples = aggregator.flush(168);
        assert!(samples.iter().any(|s| s.name == "web.visits.total" && s.value == 1.0));
        assert!(samples.iter().any(|s| s.name == "web.visits.page" && s.tags.get("page") == Some(&"/subscription".to_string())));
        assert!(samples.iter().any(|s| s.name == "web.visits.host" && s.tags.get("host") == Some(&"hehe.frkn.org".to_string())));
    }
}
