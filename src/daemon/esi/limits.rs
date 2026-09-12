use std::collections::HashMap;
use std::time::{Duration, Instant};

use reqwest::header::HeaderMap;

#[derive(Debug, Default)]
pub struct RateLimits {
    global_pause_until: Option<Instant>,
    buckets: HashMap<(String, u64), Bucket>,
}

#[derive(Debug)]
struct Bucket {
    remaining: u64,
    reset_at: Instant,
}

impl RateLimits {
    pub fn observe(&mut self, character_id: u64, status: u16, headers: &HeaderMap, now: Instant) {
        let retry_after =
            duration_header(headers, "retry-after").unwrap_or(Duration::from_secs(30));
        let legacy_remaining = integer_header(headers, "x-esi-error-limit-remain");
        let legacy_reset =
            duration_header(headers, "x-esi-error-limit-reset").unwrap_or(Duration::from_secs(60));
        if status == 420 || legacy_remaining == Some(0) {
            self.global_pause_until = Some(now + legacy_reset.max(retry_after));
        } else if legacy_remaining.is_some_and(|remaining| remaining < 10) {
            self.global_pause_until = Some(now + Duration::from_secs(2));
        }
        if let Some(group) = text_header(headers, "x-ratelimit-group") {
            let remaining = integer_header(headers, "x-ratelimit-remaining").unwrap_or(0);
            let used = integer_header(headers, "x-ratelimit-used").unwrap_or(0);
            let capacity = text_header(headers, "x-ratelimit-limit")
                .and_then(|value| {
                    value
                        .split_once('/')
                        .and_then(|(limit, _)| limit.parse::<u64>().ok())
                })
                .unwrap_or(remaining.saturating_add(used));
            let reset = parse_limit_window(headers).unwrap_or(Duration::from_secs(60));
            let effective = remaining.min(capacity.saturating_sub(used));
            let spread = if effective > 0 && effective < capacity / 10 {
                reset / (effective as u32 + 1)
            } else {
                Duration::ZERO
            };
            self.buckets.insert(
                (group, character_id),
                Bucket {
                    remaining: if spread.is_zero() { effective } else { 0 },
                    reset_at: now + if effective == 0 { reset } else { spread },
                },
            );
        }
        if status == 429 {
            if let Some(group) = text_header(headers, "x-ratelimit-group") {
                self.buckets.insert(
                    (group, character_id),
                    Bucket {
                        remaining: 0,
                        reset_at: now + retry_after,
                    },
                );
            } else {
                self.global_pause_until = Some(now + retry_after);
            }
        }
    }

    pub fn allowed_at(&self, character_id: u64, now: Instant) -> Instant {
        let mut deadline = self
            .global_pause_until
            .filter(|until| *until > now)
            .unwrap_or(now);
        for ((_, id), bucket) in &self.buckets {
            if *id == character_id && bucket.remaining == 0 && bucket.reset_at > deadline {
                deadline = bucket.reset_at;
            }
        }
        deadline
    }
}

fn text_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(str::to_owned)
}
fn integer_header(headers: &HeaderMap, name: &str) -> Option<u64> {
    text_header(headers, name)?.parse().ok()
}
fn duration_header(headers: &HeaderMap, name: &str) -> Option<Duration> {
    integer_header(headers, name).map(Duration::from_secs)
}
fn parse_limit_window(headers: &HeaderMap) -> Option<Duration> {
    let raw = text_header(headers, "x-ratelimit-limit")?;
    let (_, window) = raw.split_once('/')?;
    let unit = window.chars().last()?;
    let value: u64 = window[..window.len() - 1].parse().ok()?;
    Some(Duration::from_secs(
        value
            * match unit {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                _ => return None,
            },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderName, HeaderValue};
    fn headers(values: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (name, value) in values {
            h.insert(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
        }
        h
    }
    #[test]
    fn legacy_exhaustion_and_420_pause_everyone() {
        let now = Instant::now();
        let mut limits = RateLimits::default();
        limits.observe(
            1,
            200,
            &headers(&[
                ("x-esi-error-limit-remain", "0"),
                ("x-esi-error-limit-reset", "45"),
            ]),
            now,
        );
        assert_eq!(limits.allowed_at(2, now), now + Duration::from_secs(45));
        limits.observe(1, 420, &headers(&[("retry-after", "90")]), now);
        assert_eq!(limits.allowed_at(2, now), now + Duration::from_secs(90));
    }
    #[test]
    fn group_bucket_is_per_character_and_parses_window() {
        let now = Instant::now();
        let mut limits = RateLimits::default();
        limits.observe(
            7,
            200,
            &headers(&[
                ("x-ratelimit-group", "location"),
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-limit", "150/15m"),
            ]),
            now,
        );
        assert_eq!(limits.allowed_at(7, now), now + Duration::from_secs(900));
        assert_eq!(limits.allowed_at(8, now), now);
    }
    #[test]
    fn headerless_429_still_pauses() {
        let now = Instant::now();
        let mut limits = RateLimits::default();
        limits.observe(1, 429, &HeaderMap::new(), now);
        assert_eq!(limits.allowed_at(9, now), now + Duration::from_secs(30));
    }
}
