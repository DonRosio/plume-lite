use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use plume_lite_protocol::Snowflake;

/// In-memory token bucket keyed by client (IP or user id).
///
/// Single-node only: when the API runs on several instances, move the buckets to
/// Redis (same algorithm, `EVAL` script) so limits are global.
pub struct RateLimiter<K> {
    capacity: f64,
    refill_per_sec: f64,
    buckets: Mutex<HashMap<K, Bucket>>,
}

struct Bucket {
    tokens: f64,
    updated: Instant,
}

impl<K: Eq + Hash> RateLimiter<K> {
    /// Allows bursts of `burst` requests, refilled at `burst` per `period`.
    pub fn new(burst: u32, period: Duration) -> Self {
        let capacity = f64::from(burst.max(1));
        Self { capacity, refill_per_sec: capacity / period.as_secs_f64(), buckets: Mutex::new(HashMap::new()) }
    }

    /// Consumes one token, or returns how long to wait before retrying.
    pub fn check(&self, key: K) -> Result<(), Duration> {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let bucket = buckets.entry(key).or_insert(Bucket { tokens: self.capacity, updated: now });
        let elapsed = now.duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - bucket.tokens) / self.refill_per_sec))
        }
    }

    /// Forgets buckets that are full again (they behave exactly like absent ones).
    pub fn prune(&self) {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        buckets.retain(|_, b| {
            b.tokens + now.duration_since(b.updated).as_secs_f64() * self.refill_per_sec < self.capacity
        });
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets.lock().unwrap().len()
    }
}

/// All the limiters of the API. Numbers are deliberately close to Discord's public limits.
pub struct Limits {
    pub enabled: bool,
    /// Every request, per IP.
    pub global: RateLimiter<IpAddr>,
    /// Register / login, per IP (slows down credential stuffing).
    pub auth: RateLimiter<IpAddr>,
    /// Message creation, per user.
    pub messages: RateLimiter<Snowflake>,
    /// Typing notifications, per user.
    pub typing: RateLimiter<Snowflake>,
    /// Adding or removing reactions, per user.
    pub reactions: RateLimiter<Snowflake>,
    /// Searches (heavier than a page of history), per user.
    pub search: RateLimiter<Snowflake>,
}

impl Limits {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            global: RateLimiter::new(100, Duration::from_secs(5)),
            auth: RateLimiter::new(10, Duration::from_secs(60)),
            messages: RateLimiter::new(5, Duration::from_secs(5)),
            typing: RateLimiter::new(5, Duration::from_secs(10)),
            reactions: RateLimiter::new(10, Duration::from_secs(5)),
            search: RateLimiter::new(10, Duration::from_secs(10)),
        }
    }

    pub fn check<K: Eq + Hash>(&self, limiter: &RateLimiter<K>, key: K) -> Result<(), Duration> {
        if self.enabled { limiter.check(key) } else { Ok(()) }
    }

    pub fn prune(&self) {
        self.global.prune();
        self.auth.prune();
        self.messages.prune();
        self.typing.prune();
        self.reactions.prune();
        self.search.prune();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_burst_then_limits() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        assert!(limiter.check(1).is_ok());
        assert!(limiter.check(1).is_ok());
        assert!(limiter.check(1).is_ok());
        let retry = limiter.check(1).unwrap_err();
        assert!(retry > Duration::from_secs(10) && retry <= Duration::from_secs(20));
        assert!(limiter.check(2).is_ok(), "keys are independent");
    }

    #[test]
    fn refills_and_prunes() {
        let limiter = RateLimiter::new(1, Duration::from_millis(20));
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_err());
        std::thread::sleep(Duration::from_millis(30));
        assert!(limiter.check("a").is_ok());
        std::thread::sleep(Duration::from_millis(30));
        limiter.prune();
        assert_eq!(limiter.len(), 0);
    }
}
