use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::time::{Instant, sleep};
use tracing::trace;

/// Internal state for the token bucket
struct TokenBucketInner {
    /// Current tokens (in micro-units: 1,000,000 = 1 token)
    tokens_micro: AtomicU64,
    /// Refill rate (micro-tokens per second)
    refill_rate_micro: AtomicU64,
    /// Last refill timestamp (milliseconds since creation)
    last_refill_ms: AtomicU64,
    /// Base instant for time calculations
    base_instant: Instant,
}

/// Token bucket for rate limiting
/// Uses atomic operations for lock-free concurrent access
#[derive(Clone)]
pub struct TokenBucket {
    inner: Arc<TokenBucketInner>,
}

impl TokenBucket {
    /// Micro-units per token (for precision without floating point)
    const MICRO_UNITS: u64 = 1_000_000;

    /// Create a new token bucket
    ///
    /// # Arguments
    /// * `init_tokens` - Initial number of tokens (will be converted to micro-units)
    /// * `refill_rate` - Tokens per second (will be converted to micro-units)
    pub fn new(init_tokens: f64, refill_rate: f64) -> Self {
        let tokens_micro = (init_tokens * Self::MICRO_UNITS as f64) as u64;
        let refill_rate_micro = (refill_rate * Self::MICRO_UNITS as f64) as u64;
        let base_instant = Instant::now();

        TokenBucket {
            inner: Arc::new(TokenBucketInner {
                tokens_micro: AtomicU64::new(tokens_micro),
                refill_rate_micro: AtomicU64::new(refill_rate_micro),
                last_refill_ms: AtomicU64::new(0),
                base_instant,
            }),
        }
    }

    /// Consume tokens from the bucket
    ///
    /// This method will wait if not enough tokens are available.
    /// If refill_rate is 0, it returns immediately without consuming tokens.
    pub async fn consume(&self, amount: usize) {
        let refill_rate_micro = self.inner.refill_rate_micro.load(Ordering::Relaxed);

        // If refill rate is 0, skip token consumption
        if refill_rate_micro == 0 {
            trace!("Token bucket refill rate is 0, skipping token consumption");
            return;
        }

        let amount_micro = (amount as u64) * Self::MICRO_UNITS;

        loop {
            // Get current time in milliseconds since base
            let now = Instant::now();
            let now_ms = now.duration_since(self.inner.base_instant).as_millis() as u64;

            // Try to refill and consume atomically
            let result = self.try_consume_atomic(amount_micro, now_ms);

            match result {
                Ok(()) => {
                    // Successfully consumed tokens
                    return;
                }
                Err(wait_ms) => {
                    // Not enough tokens, wait and retry
                    if wait_ms > 0 {
                        sleep(Duration::from_millis(wait_ms)).await;
                    }
                    // Continue loop to retry
                }
            }
        }
    }

    /// Try to consume tokens atomically using proper CAS loop
    ///
    /// Returns Ok(()) if successful, Err(wait_ms) if need to wait
    fn try_consume_atomic(&self, amount_micro: u64, now_ms: u64) -> Result<(), u64> {
        let refill_rate_micro = self.inner.refill_rate_micro.load(Ordering::Acquire);
        let last_ms = self.inner.last_refill_ms.load(Ordering::Acquire);

        // Calculate elapsed time and potential new tokens
        let elapsed_ms = now_ms.saturating_sub(last_ms);
        let new_tokens_micro = self.calculate_new_tokens(elapsed_ms, refill_rate_micro);

        // Use CAS loop for atomic updates
        let mut retries = 0;
        loop {
            if retries > 10 {
                // Too many retries, back off
                return Err(1);
            }

            let current_tokens = self.inner.tokens_micro.load(Ordering::Acquire);
            let updated_tokens = current_tokens.saturating_add(new_tokens_micro);

            // Check if we have enough tokens
            if updated_tokens < amount_micro {
                // Not enough tokens, calculate wait time
                let needed_micro = amount_micro - updated_tokens;
                let wait_ms = if refill_rate_micro > 0 {
                    ((needed_micro * 1000) / refill_rate_micro).max(1)
                } else {
                    1000 // Wait 1 second if no refill rate
                };

                // Best effort update of tokens and timestamp for next calculation
                let _ = self.inner.tokens_micro.compare_exchange_weak(
                    current_tokens,
                    updated_tokens,
                    Ordering::Release,
                    Ordering::Relaxed,
                );
                self.inner.last_refill_ms.store(now_ms, Ordering::Release);

                return Err(wait_ms);
            }

            // Try to consume tokens
            let final_tokens = updated_tokens - amount_micro;

            // Use compare_exchange_weak for better performance in loops
            match self.inner.tokens_micro.compare_exchange_weak(
                current_tokens,
                final_tokens,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // Successfully consumed tokens, update timestamp
                    self.inner.last_refill_ms.store(now_ms, Ordering::Release);
                    return Ok(());
                }
                Err(_) => {
                    // Another thread modified tokens, retry
                    retries += 1;
                    std::hint::spin_loop();
                    continue;
                }
            }
        }
    }

    /// Calculate new tokens to add based on elapsed time and refill rate
    fn calculate_new_tokens(&self, elapsed_ms: u64, refill_rate_micro: u64) -> u64 {
        if elapsed_ms == 0 || refill_rate_micro == 0 {
            return 0;
        }

        // Calculate new tokens: (elapsed_ms * refill_rate_micro) / 1000
        // Handle overflow by dividing first for very large elapsed times
        if elapsed_ms > 1_000_000 {
            (elapsed_ms / 1000) * refill_rate_micro
        } else {
            (elapsed_ms * refill_rate_micro) / 1000
        }
    }

    /// Get current number of tokens (for testing/debugging)
    /// This also triggers a refill calculation to get the most up-to-date value
    #[allow(dead_code)]
    pub fn available_tokens(&self) -> f64 {
        // Get current time
        let now = Instant::now();
        let now_ms = now.duration_since(self.inner.base_instant).as_millis() as u64;

        // Load current values
        let current_tokens = self.inner.tokens_micro.load(Ordering::Relaxed);
        let last_ms = self.inner.last_refill_ms.load(Ordering::Relaxed);
        let refill_rate_micro = self.inner.refill_rate_micro.load(Ordering::Relaxed);

        // Calculate elapsed time and new tokens
        let elapsed_ms = now_ms.saturating_sub(last_ms);
        let new_tokens_micro = if elapsed_ms > 0 && refill_rate_micro > 0 {
            // Calculate new tokens: (elapsed_ms * refill_rate_micro) / 1000
            if elapsed_ms > 1_000_000 {
                // For very large elapsed times, divide first to avoid overflow
                (elapsed_ms / 1000) * refill_rate_micro
            } else {
                (elapsed_ms * refill_rate_micro) / 1000
            }
        } else {
            0
        };

        // Add new tokens to current tokens
        let total_tokens = current_tokens.saturating_add(new_tokens_micro);

        (total_tokens as f64) / (Self::MICRO_UNITS as f64)
    }

    /// Set refill rate (for dynamic adjustment)
    #[allow(dead_code)]
    pub fn set_refill_rate(&self, rate: f64) {
        let rate_micro = (rate * Self::MICRO_UNITS as f64) as u64;
        self.inner
            .refill_rate_micro
            .store(rate_micro, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_token_bucket_consume() {
        // Create bucket with 10 tokens and 5 tokens/second refill
        let bucket = TokenBucket::new(10.0, 5.0);

        // Should be able to consume 5 tokens immediately
        bucket.consume(5).await;

        // Available tokens should be around 5
        let available = bucket.available_tokens();
        assert!((4.0..=6.0).contains(&available));
    }

    #[tokio::test]
    async fn test_token_bucket_refill() {
        // Create bucket with 0 tokens and 10 tokens/second refill
        let bucket = TokenBucket::new(0.0, 10.0);

        // Wait 100ms
        sleep(Duration::from_millis(100)).await;

        // Trigger refill by trying to consume 0 tokens
        bucket.consume(0).await;

        // Should have accumulated about 1 token (10 tokens/sec * 0.1 sec)
        let available = bucket.available_tokens();
        assert!(available >= 0.5);
    }

    #[tokio::test]
    async fn test_token_bucket_zero_rate() {
        // Create bucket with 0 refill rate
        let bucket = TokenBucket::new(10.0, 0.0);

        // Should return immediately without consuming
        bucket.consume(100).await;

        // Tokens should remain unchanged
        let available = bucket.available_tokens();
        assert_eq!(available, 10.0);
    }

    #[tokio::test]
    async fn test_token_bucket_concurrent() {
        use tokio::task;

        // Create bucket with limited tokens
        let bucket = TokenBucket::new(20.0, 10.0);

        // Spawn multiple tasks trying to consume tokens
        let bucket1 = bucket.clone();
        let handle1 = task::spawn(async move {
            bucket1.consume(5).await;
        });

        let bucket2 = bucket.clone();
        let handle2 = task::spawn(async move {
            bucket2.consume(5).await;
        });

        let bucket3 = bucket.clone();
        let handle3 = task::spawn(async move {
            bucket3.consume(5).await;
        });

        // Wait for all tasks
        let _ = tokio::join!(handle1, handle2, handle3);

        // All tasks should complete successfully
    }
}
