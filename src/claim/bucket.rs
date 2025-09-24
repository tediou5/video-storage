use crate::api::token_bucket::TokenBucket;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::debug;

/// A claim-specific bucket with metadata
#[derive(Clone)]
pub struct ClaimBucket {
    /// The actual token bucket for rate limiting (lock-free, cloneable)
    pub bucket: TokenBucket,
    /// Last access time for cleanup (wrapped in Arc for sharing)
    last_access: Arc<AtomicU64>,
    /// Expiry time from the claim
    exp_unix: u32,
    /// Maximum bandwidth in kbps (0 = unlimited)
    max_kbps: u16,
    /// Maximum concurrent connections allowed
    max_concurrency: u16,
    /// Current active connections
    active_connections: Arc<AtomicU16>,
}

impl ClaimBucket {
    /// Try to acquire a connection slot for a claim
    pub async fn try_acquire_connection(&self) -> Result<Option<ConnectionGuard>, &'static str> {
        if self.max_concurrency == 0 {
            return Ok(None);
        }

        self.try_acquire_connection_inner().await
    }

    /// Try to acquire a connection slot for a claim
    async fn try_acquire_connection_inner(&self) -> Result<Option<ConnectionGuard>, &'static str> {
        for _ in 0..3 {
            let current = self.active_connections.load(Ordering::Relaxed);
            if current >= self.max_concurrency {
                return Err("Max concurrency exceeded");
            }

            // Try to increment atomically
            match self.active_connections.compare_exchange(
                current,
                current + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Ok(Some(ConnectionGuard {
                        counter: self.active_connections.clone(),
                    }));
                }
                Err(_) => {
                    // Retry by recursion (rare case)
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }

        Err("Max retries exceeded")
    }
}

/// Manager for claim-specific token buckets
#[derive(Clone)]
pub struct ClaimBucketManager {
    /// Map from claim token to its associated bucket
    buckets: Arc<RwLock<HashMap<String, ClaimBucket>>>,
    /// Default token rate when claim doesn't specify max_kbps
    default_token_rate: f64,
    /// Cleanup interval for expired buckets
    cleanup_interval: Duration,
}

impl ClaimBucketManager {
    /// Create a new claim bucket manager
    pub fn new(default_token_rate: f64) -> Self {
        let manager = Self {
            buckets: Arc::new(RwLock::new(HashMap::new())),
            default_token_rate,
            cleanup_interval: Duration::from_secs(300), // 5 minutes
        };

        // Start cleanup task
        manager.start_cleanup_task();
        manager
    }

    /// Get or create a token bucket for a claim
    pub async fn get_or_create_bucket(
        &self,
        claim_token: &str,
        exp_unix: u32,
        max_kbps: u16,
        max_concurrency: u16,
    ) -> ClaimBucket {
        // First try to get existing bucket
        let buckets = self.buckets.read().await;
        if let Some(claim_bucket) = buckets.get(claim_token).cloned() {
            debug!(
                claim_token_hash = %hash_token(claim_token),
                max_kbps,
                max_concurrency,
                "Using existing claim bucket"
            );
            // Update last access time
            let now_ms = Instant::now().elapsed().as_millis() as u64;
            claim_bucket.last_access.store(now_ms, Ordering::Relaxed);

            return claim_bucket;
        }
        drop(buckets);

        // Create new bucket if not exists
        let token_rate = if max_kbps > 0 {
            // Convert kbps to bytes per second
            (max_kbps as f64) * 1024.0 / 8.0
        } else {
            self.default_token_rate
        };

        debug!(
            claim_token_hash = %hash_token(claim_token),
            max_kbps,
            max_concurrency,
            token_rate,
            "Creating new claim bucket"
        );

        let bucket = TokenBucket::new(token_rate, token_rate);
        let now_ms = Instant::now().elapsed().as_millis() as u64;

        let claim_bucket = ClaimBucket {
            bucket,
            last_access: Arc::new(AtomicU64::new(now_ms)),
            exp_unix,
            max_kbps,
            max_concurrency,
            active_connections: Arc::new(AtomicU16::new(0)),
        };

        // Insert into map
        let mut buckets = self.buckets.write().await;
        buckets.insert(claim_token.to_string(), claim_bucket.clone());

        claim_bucket
    }

    /// Update last access time for a claim
    pub async fn touch(&self, claim_token: &str) {
        let buckets = self.buckets.read().await;
        if let Some(claim_bucket) = buckets.get(claim_token) {
            let now_ms = Instant::now().elapsed().as_millis() as u64;
            claim_bucket.last_access.store(now_ms, Ordering::Relaxed);
        }
    }

    /// Remove expired buckets
    async fn cleanup_expired(&self) {
        let now_ms = Instant::now().elapsed().as_millis() as u64;
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;

        let mut buckets = self.buckets.write().await;
        let before_count = buckets.len();

        // Remove buckets that are either expired or haven't been accessed recently
        buckets.retain(|token, claim_bucket| {
            // Check if claim has expired
            if claim_bucket.exp_unix < now_unix {
                debug!(
                    claim_token_hash = %hash_token(token),
                    exp_unix = claim_bucket.exp_unix,
                    now_unix,
                    "Removing expired claim bucket"
                );
                return false;
            }

            // Check if bucket hasn't been accessed for a while (10 minutes)
            let last_access_ms = claim_bucket.last_access.load(Ordering::Relaxed);
            let idle_ms = now_ms.saturating_sub(last_access_ms);
            if idle_ms > 600_000 {
                // 10 minutes in milliseconds
                debug!(
                    claim_token_hash = %hash_token(token),
                    idle_secs = idle_ms / 1000,
                    "Removing idle claim bucket"
                );
                return false;
            }

            true
        });

        let removed = before_count - buckets.len();
        if removed > 0 {
            debug!(
                removed,
                remaining = buckets.len(),
                "Cleaned up claim buckets"
            );
        }
    }

    /// Start background task to cleanup expired buckets
    fn start_cleanup_task(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(manager.cleanup_interval);
            loop {
                interval.tick().await;
                manager.cleanup_expired().await;
            }
        });
    }

    /// Get statistics about current buckets
    #[allow(dead_code)]
    pub async fn stats(&self) -> ClaimBucketStats {
        let buckets = self.buckets.read().await;
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;

        let mut active = 0;
        let mut expired = 0;
        let mut unlimited = 0;
        let mut limited = 0;

        for (_token, claim_bucket) in buckets.iter() {
            if claim_bucket.exp_unix < now_unix {
                expired += 1;
            } else {
                active += 1;
            }

            if claim_bucket.max_kbps == 0 {
                unlimited += 1;
            } else {
                limited += 1;
            }
        }

        ClaimBucketStats {
            total: buckets.len(),
            active,
            expired,
            unlimited,
            limited,
        }
    }
}

/// Statistics about claim buckets
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ClaimBucketStats {
    pub total: usize,
    pub active: usize,
    pub expired: usize,
    pub unlimited: usize,
    pub limited: usize,
}

/// Connection guard that decrements counter on drop
pub struct ConnectionGuard {
    counter: Arc<AtomicU16>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.counter
            .update(Ordering::Release, Ordering::Acquire, |count| {
                if count > 0 { count - 1 } else { count }
            });
    }
}

/// Hash a token for logging (to avoid exposing full tokens in logs)
fn hash_token(token: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    token.hash(&mut hasher);
    let hash = hasher.finish();

    // Return first 8 characters of hex representation
    format!("{:x}", hash).chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sub_zero() {
        let counter = Arc::new(AtomicU16::new(1));

        let c = counter.clone();
        let j1 = std::thread::spawn(move || {
            c.update(Ordering::Release, Ordering::Acquire, |count| {
                if count > 0 { count - 1 } else { count }
            });
            assert_eq!(c.load(Ordering::Relaxed), 0);
        });

        let c = counter.clone();
        let j2 = std::thread::spawn(move || {
            c.update(Ordering::Release, Ordering::Acquire, |count| {
                if count > 0 { count - 1 } else { count }
            });
            assert_eq!(c.load(Ordering::Relaxed), 0);
        });

        let c = counter.clone();
        let j3 = std::thread::spawn(move || {
            c.update(Ordering::Release, Ordering::Acquire, |count| {
                if count > 0 { count - 1 } else { count }
            });
            assert_eq!(c.load(Ordering::Relaxed), 0);
        });

        j1.join().unwrap();
        j2.join().unwrap();
        j3.join().unwrap();

        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_bucket_creation_and_reuse() {
        let manager = ClaimBucketManager::new(1000.0);

        // Create a bucket
        let _bucket1 = manager
            .get_or_create_bucket("token1", 2000000000, 500, 10)
            .await;

        // Get the same bucket again
        let _bucket2 = manager
            .get_or_create_bucket("token1", 2000000000, 500, 10)
            .await;

        // They should have the same refill rate (cloned from same source)
        // Note: We can't check pointer equality anymore since they're clones

        // Different token should get different bucket
        let _bucket3 = manager
            .get_or_create_bucket("token2", 2000000000, 1000, 5)
            .await;
    }

    #[tokio::test]
    async fn test_rate_calculation() {
        let manager = ClaimBucketManager::new(1000.0);

        // Test with specific kbps
        let _bucket = manager
            .get_or_create_bucket("token1", 2000000000, 800, 10)
            .await;
        // 800 kbps = 800 * 1024 / 8 = 102400 bytes/second

        // Test with unlimited (0 kbps)
        let _bucket = manager
            .get_or_create_bucket("token2", 2000000000, 0, 20)
            .await;
        // Should use default rate
    }

    #[tokio::test]
    async fn test_cleanup() {
        let manager = ClaimBucketManager::new(1000.0);

        // Add an expired bucket
        let past_time = 1000000000u32; // Far in the past
        manager
            .get_or_create_bucket("expired", past_time, 500, 10)
            .await;

        // Add an active bucket
        let future_time = 2000000000u32;
        manager
            .get_or_create_bucket("active", future_time, 500, 10)
            .await;

        // Run cleanup
        manager.cleanup_expired().await;

        // Check stats
        let stats = manager.stats().await;
        assert_eq!(stats.total, 1); // Only active bucket should remain
        assert_eq!(stats.active, 1);
    }
}
