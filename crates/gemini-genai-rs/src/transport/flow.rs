//! Flow control — token bucket rate limiter for send pacing.
//!
//! Prevents overwhelming the WebSocket with audio faster than
//! the network or server can absorb.

use std::time::{Duration, Instant};

/// Configuration for the token bucket rate limiter.
#[derive(Debug, Clone)]
pub struct FlowConfig {
    /// Maximum tokens (bytes) in the bucket.
    pub bucket_capacity: usize,
    /// Refill rate in bytes per second.
    pub refill_rate_bps: usize,
}

impl Default for FlowConfig {
    fn default() -> Self {
        // Default: 256 kbps (16kHz × 16-bit PCM)
        Self {
            bucket_capacity: 64_000, // ~250ms burst allowance
            refill_rate_bps: 32_000, // 16kHz × 2 bytes per sample
        }
    }
}

/// Token bucket rate limiter for send pacing.
///
/// Allows bursts up to `bucket_capacity` bytes, then throttles
/// to `refill_rate_bps` sustained rate.
pub struct TokenBucket {
    config: FlowConfig,
    /// Current token count.
    tokens: f64,
    /// Last refill timestamp.
    last_refill: Instant,
}

impl TokenBucket {
    /// Create a new token bucket with the given configuration.
    pub fn new(config: FlowConfig) -> Self {
        let tokens = config.bucket_capacity as f64;
        Self {
            config,
            tokens,
            last_refill: Instant::now(),
        }
    }

    /// Try to consume `n` tokens. Returns `true` if allowed, `false` if rate-limited.
    pub fn try_consume(&mut self, n: usize) -> bool {
        self.refill();
        if self.tokens >= n as f64 {
            self.tokens -= n as f64;
            true
        } else {
            false
        }
    }

    /// How long to wait before `n` tokens are available.
    pub fn wait_duration(&mut self, n: usize) -> Duration {
        self.refill();
        if self.tokens >= n as f64 {
            Duration::ZERO
        } else {
            let deficit = n as f64 - self.tokens;
            let secs = deficit / self.config.refill_rate_bps as f64;
            Duration::from_secs_f64(secs)
        }
    }

    /// Consume tokens, waiting if necessary.
    pub async fn consume(&mut self, n: usize) {
        let wait = self.wait_duration(n);
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        self.refill();
        self.tokens -= n as f64;
        // Tokens can go negative after a long wait; that's fine,
        // subsequent calls will wait proportionally.
    }

    /// Current available tokens.
    pub fn available(&mut self) -> usize {
        self.refill();
        self.tokens.max(0.0) as usize
    }

    /// Refill tokens based on elapsed time.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill);
        self.last_refill = now;

        let added = elapsed.as_secs_f64() * self.config.refill_rate_bps as f64;
        self.tokens = (self.tokens + added).min(self.config.bucket_capacity as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_burst() {
        let mut bucket = TokenBucket::new(FlowConfig {
            bucket_capacity: 1000,
            refill_rate_bps: 100,
        });

        // Should allow up to capacity
        assert!(bucket.try_consume(1000));
        // Should be empty now
        assert!(!bucket.try_consume(1));
    }

    #[test]
    fn refill_over_time() {
        let mut bucket = TokenBucket::new(FlowConfig {
            bucket_capacity: 1000,
            refill_rate_bps: 1000,
        });

        bucket.try_consume(1000); // drain it
        assert!(!bucket.try_consume(1));

        // Manually advance time by setting last_refill in the past
        bucket.last_refill = Instant::now() - Duration::from_secs(1);
        assert!(bucket.try_consume(500)); // ~1000 tokens refilled
    }

    #[test]
    fn wait_duration_calculation() {
        let mut bucket = TokenBucket::new(FlowConfig {
            bucket_capacity: 1000,
            refill_rate_bps: 100,
        });

        bucket.try_consume(1000); // drain
        let wait = bucket.wait_duration(100);
        // Need 100 tokens at 100/sec = 1 second
        assert!(wait >= Duration::from_millis(900));
        assert!(wait <= Duration::from_millis(1100));
    }
}
