use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use reqwest::header::HeaderMap;

#[derive(Debug, Clone)]
pub struct CacheEntry<T> {
    pub body: T,
    pub etag: Option<String>,
    pub fresh_until: Instant,
}

#[derive(Debug)]
pub struct ResponseCache<T> {
    entries: HashMap<(u64, &'static str), CacheEntry<T>>,
}

impl<T> Default for ResponseCache<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

impl<T: Clone> ResponseCache<T> {
    pub fn fresh_until(&self, character_id: u64, endpoint: &'static str) -> Option<Instant> {
        self.entries
            .get(&(character_id, endpoint))
            .map(|entry| entry.fresh_until)
    }
    pub fn remove_character(&mut self, character_id: u64) {
        self.entries.retain(|(id, _), _| *id != character_id);
    }
    pub fn get_fresh(&self, character_id: u64, endpoint: &'static str, now: Instant) -> Option<T> {
        self.entries
            .get(&(character_id, endpoint))
            .filter(|entry| now < entry.fresh_until)
            .map(|entry| entry.body.clone())
    }
    pub fn etag_after_expiry(
        &self,
        character_id: u64,
        endpoint: &'static str,
        now: Instant,
    ) -> Option<&str> {
        self.entries
            .get(&(character_id, endpoint))
            .filter(|entry| now >= entry.fresh_until)
            .and_then(|entry| entry.etag.as_deref())
    }
    pub fn store(
        &mut self,
        character_id: u64,
        endpoint: &'static str,
        body: T,
        headers: &HeaderMap,
        now: Instant,
        wall_now: SystemTime,
    ) {
        self.entries.insert(
            (character_id, endpoint),
            CacheEntry {
                body,
                etag: header(headers, "etag"),
                fresh_until: freshness(headers, now, wall_now),
            },
        );
    }
    pub fn revalidate_304(
        &mut self,
        character_id: u64,
        endpoint: &'static str,
        headers: &HeaderMap,
        now: Instant,
        wall_now: SystemTime,
    ) -> Option<T> {
        let entry = self.entries.get_mut(&(character_id, endpoint))?;
        entry.fresh_until = freshness(headers, now, wall_now);
        if let Some(etag) = header(headers, "etag") {
            entry.etag = Some(etag);
        }
        Some(entry.body.clone())
    }
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(str::to_owned)
}

pub fn freshness(headers: &HeaderMap, now: Instant, wall_now: SystemTime) -> Instant {
    if let Some(max_age) = header(headers, "cache-control").and_then(|value| {
        value
            .split(',')
            .find_map(|part| part.trim().strip_prefix("max-age=")?.parse::<u64>().ok())
    }) {
        let age = header(headers, "age")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        return now + Duration::from_secs(max_age.saturating_sub(age));
    }
    if let Some(expires) =
        header(headers, "expires").and_then(|value| httpdate::parse_http_date(&value).ok())
    {
        let base = header(headers, "date")
            .and_then(|value| httpdate::parse_http_date(&value).ok())
            .unwrap_or(wall_now);
        return now + expires.duration_since(base).unwrap_or(Duration::ZERO);
    }
    now
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{CACHE_CONTROL, ETAG, HeaderValue};
    #[test]
    fn honors_freshness_etag_and_304() {
        let start = Instant::now();
        let wall = SystemTime::now();
        let mut headers = HeaderMap::new();
        headers.insert(
            CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=120"),
        );
        headers.insert(ETAG, HeaderValue::from_static("\"one\""));
        let mut cache = ResponseCache::default();
        cache.store(7, "implants", vec![1], &headers, start, wall);
        assert_eq!(
            cache.get_fresh(7, "implants", start + Duration::from_secs(119)),
            Some(vec![1])
        );
        assert_eq!(
            cache.etag_after_expiry(7, "implants", start + Duration::from_secs(120)),
            Some("\"one\"")
        );
        assert_eq!(
            cache.revalidate_304(
                7,
                "implants",
                &headers,
                start + Duration::from_secs(120),
                wall
            ),
            Some(vec![1])
        );
    }
}
