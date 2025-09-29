use crate::bucket::{ClaimBucket, ClaimBucketStats};
use crate::error::ClaimError;
use crate::header::{ALG_AES_256_GCM, ClaimHeader, HEADER_SIZE, TAG_SIZE};
use crate::payload_v1::ClaimPayloadV1;
use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Key, Nonce};
use anyhow::{Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::debug;

/// Unified claim manager for key management, token operations, and bucket management
#[derive(Debug)]
struct ClaimManagerInner {
    buckets: HashMap<[u8; 28], ClaimBucket>,
    keys: HashMap<u8, [u8; 32]>,
    current_kid: u8,
}

impl ClaimManagerInner {
    fn new() -> Self {
        let mut keys = HashMap::new();

        // Generate initial key for kid=1
        let mut key = [0u8; 32];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut key);
        keys.insert(1, key);

        Self {
            keys,
            current_kid: 1,
            buckets: HashMap::new(),
        }
    }

    fn from_keys(keys: Vec<(u8, [u8; 32])>) -> Self {
        if keys.is_empty() {
            return Self::new();
        };

        let keys = keys.into_iter().collect::<HashMap<_, _>>();
        let current_kid = keys.keys().min().copied().expect("Must has key");

        Self {
            keys,
            current_kid,
            buckets: HashMap::new(),
        }
    }

    /// Create a new claim manager with a specific key (for testing or key rotation)
    #[allow(dead_code)]
    fn with_key(kid: u8, key: [u8; 32]) -> Self {
        let mut keys = HashMap::new();
        keys.insert(kid, key);

        Self {
            keys,
            current_kid: kid,
            buckets: HashMap::new(),
        }
    }

    /// Add a new key for rotation
    #[allow(dead_code)]
    fn add_key(&mut self, kid: u8, key: [u8; 32]) -> Result<()> {
        if self.keys.contains_key(&kid) {
            return Err(anyhow!("Key ID {} already exists", kid));
        }
        self.keys.insert(kid, key);
        Ok(())
    }

    /// Set the current key ID for signing new tokens
    #[allow(dead_code)]
    fn set_current_kid(&mut self, kid: u8) -> Result<()> {
        if !self.keys.contains_key(&kid) {
            return Err(anyhow!("Key ID {} not found", kid));
        }
        self.current_kid = kid;
        Ok(())
    }

    /// Sign a claim and return the base64url-encoded token
    fn sign_claim(&self, payload: &ClaimPayloadV1) -> Result<String> {
        let key = self
            .keys
            .get(&self.current_kid)
            .ok_or_else(|| anyhow!("Key ID {} not found", self.current_kid))?;

        // Create header
        let header = ClaimHeader::new(self.current_kid, ALG_AES_256_GCM);
        let header_bytes = header.to_bytes();

        // Serialize payload
        let payload_bytes = payload.serialize_to_bytes()?;

        // Encrypt using AES-256-GCM
        let cipher_key = Key::<Aes256Gcm>::from_slice(key);
        let cipher = Aes256Gcm::new(cipher_key);
        let nonce = Nonce::from_slice(&header.nonce);

        // Use header as AAD (Additional Authenticated Data)
        let ciphertext = cipher
            .encrypt(
                nonce,
                aes_gcm::aead::Payload {
                    msg: &payload_bytes,
                    aad: &header_bytes,
                },
            )
            .map_err(|error| anyhow!("Encryption failed: {error}"))?;

        // Combine: header || ciphertext (includes tag)
        let mut token_bytes = header_bytes;
        token_bytes.extend_from_slice(&ciphertext);

        // Encode as base64url without padding
        Ok(URL_SAFE_NO_PAD.encode(token_bytes))
    }

    /// Verify and decode a claim token, returning both payload and bucket key
    fn verify_claim(&self, token: &str) -> Result<(ClaimPayloadV1, [u8; 28]), ClaimError> {
        // Decode from base64url
        let token_bytes = URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| ClaimError::InvalidToken)?;

        if token_bytes.len() < HEADER_SIZE + TAG_SIZE {
            return Err(ClaimError::InvalidToken);
        }

        // Parse header
        let header = ClaimHeader::from_bytes(&token_bytes[..HEADER_SIZE])?;

        // Get the key
        let key = self
            .keys
            .get(&header.kid)
            .ok_or(ClaimError::KeyNotFound(header.kid))?;

        // Extract ciphertext (includes tag)
        let ciphertext = &token_bytes[HEADER_SIZE..];

        // Decrypt using AES-256-GCM
        if header.alg != ALG_AES_256_GCM {
            return Err(ClaimError::InvalidHeader(format!(
                "Unsupported algorithm: {}",
                header.alg
            )));
        }

        let cipher_key = Key::<Aes256Gcm>::from_slice(key);
        let cipher = Aes256Gcm::new(cipher_key);
        let nonce = Nonce::from_slice(&header.nonce);

        // Use header as AAD
        let header_bytes = header.to_bytes();
        let payload_bytes = cipher
            .decrypt(
                nonce,
                aes_gcm::aead::Payload {
                    msg: ciphertext,
                    aad: &header_bytes,
                },
            )
            .map_err(|_| ClaimError::AeadFail)?;

        // Deserialize payload
        let payload = ClaimPayloadV1::deserialize_from_bytes(&payload_bytes)?;

        Ok((payload, header.bucket_key()))
    }

    /// Get or create a token bucket for a claim
    fn create_bucket(
        &mut self,
        bucket_key: &[u8; 28],
        claim_payload: &ClaimPayloadV1,
        default_token_rate: f64,
    ) -> ClaimBucket {
        let token_rate = if claim_payload.max_kbps > 0 {
            // Convert kbps to bytes per second
            (claim_payload.max_kbps as f64) * 1024.0 / 8.0
        } else {
            default_token_rate
        };

        debug!(
            max_kbps = claim_payload.max_kbps,
            max_concurrency = claim_payload.max_concurrency,
            token_rate,
            "Creating new claim bucket"
        );

        let claim_bucket = ClaimBucket::new(
            token_rate,
            claim_payload.exp_unix,
            claim_payload.max_kbps,
            claim_payload.max_concurrency,
        );

        self.buckets.insert(*bucket_key, claim_bucket.clone());

        claim_bucket
    }

    /// Get a token bucket for a claim
    fn get_bucket(&self, bucket_key: &[u8; 28]) -> Option<ClaimBucket> {
        self.buckets.get(bucket_key).cloned()
    }

    /// Update last access time for a claim bucket
    fn touch_bucket(&self, bucket_key: &[u8; 28]) {
        if let Some(claim_bucket) = self.buckets.get(bucket_key) {
            claim_bucket.touch();
        }
    }

    /// Remove expired buckets
    fn cleanup_expired(&mut self) {
        let now_ms = Instant::now().elapsed().as_millis() as u64;
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;

        let before_count = self.buckets.len();

        // Remove buckets that are either expired or haven't been accessed recently
        self.buckets.retain(|_bucket_key, claim_bucket| {
            // Check if claim has expired
            if claim_bucket.is_expired(now_unix) {
                debug!(
                    exp_unix = claim_bucket.exp_unix(),
                    now_unix, "Removing expired claim bucket"
                );
                return false;
            }

            // Check if bucket hasn't been accessed for a while (10 minutes)
            if claim_bucket.is_idle(now_ms, 600_000) {
                debug!("Removing idle claim bucket");
                return false;
            }

            true
        });

        let removed = before_count - self.buckets.len();
        if removed > 0 {
            debug!(
                removed,
                remaining = self.buckets.len(),
                "Cleaned up claim buckets"
            );
        }
    }

    /// Get statistics about current buckets
    #[allow(dead_code)]
    fn bucket_stats(&self) -> ClaimBucketStats {
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;

        let mut active = 0;
        let mut expired = 0;
        let mut unlimited = 0;
        let mut limited = 0;

        for (_bucket_key, claim_bucket) in self.buckets.iter() {
            if claim_bucket.is_expired(now_unix) {
                expired += 1;
            } else {
                active += 1;
            }

            if claim_bucket.max_kbps() == 0 {
                unlimited += 1;
            } else {
                limited += 1;
            }
        }

        ClaimBucketStats {
            total: self.buckets.len(),
            active,
            expired,
            unlimited,
            limited,
        }
    }
}

/// Unified claim manager for key management, token operations, and bucket management
#[derive(Clone, Debug)]
pub struct ClaimManager {
    inner: Arc<RwLock<ClaimManagerInner>>,

    default_token_rate: f64,
    cleanup_interval: Duration,
}

impl ClaimManager {
    /// Create a new ClaimManager with randomly generated keys
    pub fn new(default_token_rate: f64) -> Self {
        Self {
            inner: Arc::new(RwLock::new(ClaimManagerInner::new())),
            default_token_rate,
            cleanup_interval: Duration::from_secs(300), // 5 minutes
        }
    }

    /// Create a ClaimManager from configuration
    /// If config_keys is provided, use them; otherwise generate random keys
    pub fn from_keys(keys: Vec<(u8, [u8; 32])>, default_token_rate: f64) -> Result<Self> {
        if keys.is_empty() {
            return Ok(Self::new(default_token_rate));
        };

        Ok(Self {
            inner: Arc::new(RwLock::new(ClaimManagerInner::from_keys(keys))),
            default_token_rate,
            cleanup_interval: Duration::from_secs(300),
        })
    }

    /// Create a new claim manager with a specific key (for testing or key rotation)
    #[allow(dead_code)]
    pub fn with_key(kid: u8, key: [u8; 32], default_token_rate: f64) -> Self {
        Self {
            inner: Arc::new(RwLock::new(ClaimManagerInner::with_key(kid, key))),
            default_token_rate,
            cleanup_interval: Duration::from_secs(300),
        }
    }

    /// Add a new key for rotation
    #[allow(dead_code)]
    pub fn add_key(&self, kid: u8, key: [u8; 32]) -> Result<()> {
        self.inner.write().add_key(kid, key)
    }

    /// Set the current key ID for signing new tokens
    #[allow(dead_code)]
    pub fn set_current_kid(&self, kid: u8) -> Result<()> {
        self.inner.write().set_current_kid(kid)
    }

    /// Sign a claim and return the base64url-encoded token
    pub fn sign_claim(&self, payload: &ClaimPayloadV1) -> Result<String> {
        self.inner.read().sign_claim(payload)
    }

    /// Verify and decode a claim token, returning both payload and bucket key
    pub fn verify_claim(&self, token: &str) -> Result<(ClaimPayloadV1, [u8; 28]), ClaimError> {
        self.inner.read().verify_claim(token)
    }

    /// Get or create a token bucket for a claim
    pub fn get_or_create_bucket(
        &self,
        bucket_key: &[u8; 28],
        claim_payload: &ClaimPayloadV1,
    ) -> ClaimBucket {
        if let Some(claim_bucket) = self.inner.read().get_bucket(bucket_key) {
            debug!(
                max_kbps = claim_payload.max_kbps,
                max_concurrency = claim_payload.max_concurrency,
                "Using existing claim bucket"
            );
            // Update last access time
            claim_bucket.touch();
            return claim_bucket;
        }

        self.inner
            .write()
            .create_bucket(bucket_key, claim_payload, self.default_token_rate)
    }

    /// Update last access time for a claim bucket
    pub fn touch_bucket(&self, bucket_key: &[u8; 28]) {
        self.inner.read().touch_bucket(bucket_key);
    }

    /// Start the background cleanup task
    /// This must be called after the manager is created and when in an async context
    pub fn start_cleanup_task(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(manager.cleanup_interval);
            loop {
                interval.tick().await;
                manager.inner.write().cleanup_expired();
            }
        });
    }

    /// Get statistics about current buckets
    #[allow(dead_code)]
    pub fn bucket_stats(&self) -> ClaimBucketStats {
        self.inner.read().bucket_stats()
    }
}

impl Default for ClaimManager {
    fn default() -> Self {
        Self::new(1000.0) // Default 1MB/s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claim_sign_and_verify() {
        let manager = ClaimManager::new(1000.0);

        let payload = ClaimPayloadV1 {
            exp_unix: 2000000000,
            nbf_unix: 1700000000,
            asset_id: "test789".to_string(),
            window_len_sec: 300,
            max_kbps: 5000,
            max_concurrency: 15,
            allowed_widths: vec![],
        };

        let token = manager.sign_claim(&payload).unwrap();
        assert!(!token.is_empty());

        let (verified, bucket_key) = manager.verify_claim(&token).unwrap();
        assert_eq!(verified.asset_id, payload.asset_id);
        assert_eq!(verified.exp_unix, payload.exp_unix);
        assert_eq!(verified.window_len_sec, payload.window_len_sec);
        assert_eq!(verified.max_kbps, payload.max_kbps);
        assert_eq!(verified.max_concurrency, payload.max_concurrency);
        assert!(!bucket_key.is_empty());
    }

    #[tokio::test]
    async fn test_bucket_creation_and_reuse() {
        let manager = ClaimManager::new(1000.0);

        let payload = ClaimPayloadV1 {
            exp_unix: 2000000000,
            nbf_unix: 1700000000,
            asset_id: "test_bucket".to_string(),
            window_len_sec: 300,
            max_kbps: 500,
            max_concurrency: 10,
            allowed_widths: vec![],
        };

        let token = manager.sign_claim(&payload).unwrap();
        let (verified_payload, bucket_key) = manager.verify_claim(&token).unwrap();

        // Create a bucket
        let _bucket1 = manager.get_or_create_bucket(&bucket_key, &verified_payload);

        // Get the same bucket again
        let _bucket2 = manager.get_or_create_bucket(&bucket_key, &verified_payload);

        // Different token should get different bucket
        let payload2 = ClaimPayloadV1 {
            exp_unix: 2000000000,
            nbf_unix: 1700000000,
            asset_id: "test_bucket_2".to_string(),
            window_len_sec: 300,
            max_kbps: 1000,
            max_concurrency: 5,
            allowed_widths: vec![],
        };

        let token2 = manager.sign_claim(&payload2).unwrap();
        let (verified_payload2, bucket_key2) = manager.verify_claim(&token2).unwrap();
        let _bucket3 = manager.get_or_create_bucket(&bucket_key2, &verified_payload2);

        assert_ne!(bucket_key, bucket_key2);
    }

    #[tokio::test]
    async fn test_cleanup() {
        let manager = ClaimManager::new(1000.0);

        // Add an expired bucket
        let expired_payload = ClaimPayloadV1 {
            exp_unix: 1000000000, // Far in the past
            nbf_unix: 900000000,
            asset_id: "expired".to_string(),
            window_len_sec: 300,
            max_kbps: 500,
            max_concurrency: 10,
            allowed_widths: vec![],
        };

        let token1 = manager.sign_claim(&expired_payload).unwrap();
        let (verified_expired, bucket_key1) = manager.verify_claim(&token1).unwrap();
        manager.get_or_create_bucket(&bucket_key1, &verified_expired);

        // Add an active bucket
        let active_payload = ClaimPayloadV1 {
            exp_unix: 2000000000,
            nbf_unix: 1700000000,
            asset_id: "active".to_string(),
            window_len_sec: 300,
            max_kbps: 500,
            max_concurrency: 10,
            allowed_widths: vec![],
        };

        let token2 = manager.sign_claim(&active_payload).unwrap();
        let (verified_active, bucket_key2) = manager.verify_claim(&token2).unwrap();
        manager.get_or_create_bucket(&bucket_key2, &verified_active);

        // Run cleanup
        manager.inner.write().cleanup_expired();

        // Check stats
        let stats = manager.bucket_stats();
        assert_eq!(stats.total, 1); // Only active bucket should remain
        assert_eq!(stats.active, 1);
    }

    #[test]
    fn test_claim_manager_from_config_with_keys() {
        let config_keys = vec![(1, [1u8; 32]), (3, [2u8; 32])];

        // Create manager with configured keys
        let manager = ClaimManager::from_keys(config_keys, 2000.0).unwrap();

        // Test signing and verification
        let payload = ClaimPayloadV1 {
            exp_unix: 2000000000,
            nbf_unix: 1700000000,
            asset_id: "test_config".to_string(),
            window_len_sec: 300,
            max_kbps: 5000,
            max_concurrency: 10,
            allowed_widths: vec![1920],
        };

        let token = manager.sign_claim(&payload).unwrap();
        let (verified, _bucket_key) = manager.verify_claim(&token).unwrap();

        assert_eq!(verified.asset_id, payload.asset_id);
        assert_eq!(verified.exp_unix, payload.exp_unix);
    }

    #[test]
    fn test_claim_manager_from_config_without_keys() {
        // Test fallback to random generation
        let manager = ClaimManager::from_keys(Vec::new(), 3000.0).unwrap();

        let payload = ClaimPayloadV1 {
            exp_unix: 2000000000,
            nbf_unix: 1700000000,
            asset_id: "test_random".to_string(),
            window_len_sec: 100,
            max_kbps: 3000,
            max_concurrency: 5,
            allowed_widths: vec![],
        };

        let token = manager.sign_claim(&payload).unwrap();
        let (verified, _bucket_key) = manager.verify_claim(&token).unwrap();

        assert_eq!(verified.asset_id, payload.asset_id);
    }

    #[test]
    fn test_claim_manager_key_rotation() {
        let config_keys = vec![(1, [1u8; 32]), (2, [2u8; 32])];

        let manager = ClaimManager::from_keys(config_keys, 1000.0).unwrap();

        // Sign with current key (should be kid=1)
        let payload = ClaimPayloadV1 {
            exp_unix: 2000000000,
            nbf_unix: 1700000000,
            asset_id: "test_rotation".to_string(),
            window_len_sec: 60,
            max_kbps: 2000,
            max_concurrency: 3,
            allowed_widths: vec![1280],
        };

        let token1 = manager.sign_claim(&payload).unwrap();

        // Change current key to kid=2
        manager.set_current_kid(2).unwrap();
        let token2 = manager.sign_claim(&payload).unwrap();

        // Both tokens should be verifiable
        let (verified1, _bucket_key1) = manager.verify_claim(&token1).unwrap();
        let (verified2, _bucket_key2) = manager.verify_claim(&token2).unwrap();

        assert_eq!(verified1.asset_id, payload.asset_id);
        assert_eq!(verified2.asset_id, payload.asset_id);

        // Tokens should be different (different keys used)
        assert_ne!(token1, token2);
    }
}
