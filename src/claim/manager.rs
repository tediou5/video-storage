use crate::claim::error::ClaimError;
use crate::claim::header::{ALG_AES_256_GCM, ClaimHeader, HEADER_SIZE, TAG_SIZE};
use crate::claim::payload_v1::ClaimPayloadV1;
use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Key, Nonce};
use anyhow::{Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// Claim manager for key management and token operations
#[derive(Clone, Debug)]
pub struct ClaimManager {
    // TODO: tokio::sync::RwLock is not needed here
    keys: Arc<RwLock<HashMap<u8, [u8; 32]>>>,
    current_kid: Arc<AtomicU8>,
}

impl ClaimManager {
    /// Create a new ClaimManager with randomly generated keys
    pub fn new() -> Self {
        let mut keys = HashMap::new();

        // Generate initial key for kid=1
        let mut key = [0u8; 32];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut key);
        keys.insert(1, key);

        Self {
            keys: Arc::new(RwLock::new(keys)),
            current_kid: Arc::new(AtomicU8::new(1)),
        }
    }

    /// Create a ClaimManager from configuration
    /// If config_keys is provided, use them; otherwise generate random keys
    pub fn from_config(config_keys: Vec<(u8, [u8; 32])>) -> Result<Self> {
        if config_keys.is_empty() {
            return Ok(Self::new());
        };

        let first_kid = config_keys
            .iter()
            .map(|(k, _)| k)
            .min()
            .copied()
            .expect("Must has key");

        Ok(Self {
            keys: Arc::new(RwLock::new(HashMap::from_iter(config_keys))),
            current_kid: Arc::new(AtomicU8::new(first_kid)),
        })
    }

    /// Create a new claim manager with a specific key (for testing or key rotation)
    #[allow(dead_code)]
    pub fn with_key(kid: u8, key: [u8; 32]) -> Self {
        let mut keys = HashMap::new();
        keys.insert(kid, key);

        Self {
            keys: Arc::new(RwLock::new(keys)),
            current_kid: Arc::new(AtomicU8::new(kid)),
        }
    }

    /// Add a new key for rotation
    #[allow(dead_code)]
    pub fn add_key(&self, kid: u8, key: [u8; 32]) -> Result<()> {
        let mut keys = self.keys.write();
        if keys.contains_key(&kid) {
            return Err(anyhow!("Key ID {} already exists", kid));
        }
        keys.insert(kid, key);
        Ok(())
    }

    /// Set the current key ID for signing new tokens
    #[allow(dead_code)]
    pub fn set_current_kid(&self, kid: u8) -> Result<()> {
        let keys = self.keys.read();
        if !keys.contains_key(&kid) {
            return Err(anyhow!("Key ID {} not found", kid));
        }
        self.current_kid.store(kid, Ordering::Relaxed);
        Ok(())
    }

    /// Sign a claim and return the base64url-encoded token
    pub fn sign_claim(&self, payload: &ClaimPayloadV1) -> Result<String> {
        let kid = self.current_kid.load(Ordering::Relaxed);
        let keys = self.keys.read();
        let key = keys
            .get(&kid)
            .ok_or_else(|| anyhow!("Key ID {} not found", kid))?;

        // Create header
        let header = ClaimHeader::new(kid, ALG_AES_256_GCM);
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

    /// Verify and decode a claim token
    pub fn verify_claim(&self, token: &str) -> Result<ClaimPayloadV1, ClaimError> {
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
        let keys = self.keys.read();
        let key = keys
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
        ClaimPayloadV1::deserialize_from_bytes(&payload_bytes)
    }
}

impl Default for ClaimManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claim_sign_and_verify() {
        let manager = ClaimManager::new();

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

        let verified = manager.verify_claim(&token).unwrap();
        assert_eq!(verified.asset_id, payload.asset_id);
        assert_eq!(verified.exp_unix, payload.exp_unix);
        assert_eq!(verified.window_len_sec, payload.window_len_sec);
        assert_eq!(verified.max_kbps, payload.max_kbps);
        assert_eq!(verified.max_concurrency, payload.max_concurrency);
    }

    #[test]
    fn test_claim_serialization_deserialization() {
        // Test with various values including edge cases
        let test_cases = vec![
            ClaimPayloadV1 {
                exp_unix: 2000000000,
                nbf_unix: 1700000000,
                asset_id: "test123".to_string(),
                window_len_sec: 180,
                max_kbps: 4000,
                max_concurrency: 10,
                allowed_widths: vec![1920, 1280, 854],
            },
            ClaimPayloadV1 {
                exp_unix: u32::MAX,
                nbf_unix: 0,
                asset_id: "a".to_string(),
                window_len_sec: u16::MAX,
                max_kbps: 0,
                max_concurrency: u16::MAX,
                allowed_widths: vec![],
            },
            ClaimPayloadV1 {
                exp_unix: 1234567890,
                nbf_unix: 1234567000,
                asset_id: "very_long_asset_id_that_tests_variable_length_encoding".to_string(),
                window_len_sec: 300,
                max_kbps: 8000,
                max_concurrency: 1,
                allowed_widths: vec![3840, 2560, 1920, 1280],
            },
        ];

        for payload in test_cases {
            // Test serialization and deserialization
            let serialized = payload.serialize_to_bytes().unwrap();
            let deserialized = ClaimPayloadV1::deserialize_from_bytes(&serialized).unwrap();

            assert_eq!(deserialized.exp_unix, payload.exp_unix);
            assert_eq!(deserialized.nbf_unix, payload.nbf_unix);
            assert_eq!(deserialized.asset_id, payload.asset_id);
            assert_eq!(deserialized.window_len_sec, payload.window_len_sec);
            assert_eq!(deserialized.max_kbps, payload.max_kbps);
            assert_eq!(deserialized.max_concurrency, payload.max_concurrency);
        }
    }

    #[test]
    fn test_claim_binary_format_compatibility() {
        let payload = ClaimPayloadV1 {
            exp_unix: 0x12345678,
            nbf_unix: 0x87654321,
            asset_id: "abc".to_string(),
            window_len_sec: 0x1234,
            max_kbps: 0x5678,
            max_concurrency: 0x9ABC,
            allowed_widths: vec![1920, 1280],
        };

        let bytes = payload.serialize_to_bytes().unwrap();

        // Verify binary format
        assert_eq!(&bytes[0..4], &0x12345678u32.to_le_bytes());
        assert_eq!(&bytes[4..8], &0x87654321u32.to_le_bytes());
        assert_eq!(bytes[8], 3); // asset_id length
        assert_eq!(&bytes[9..12], b"abc");
        assert_eq!(&bytes[12..14], &0x1234u16.to_le_bytes());
        assert_eq!(&bytes[14..16], &0x5678u16.to_le_bytes());
        assert_eq!(&bytes[16..18], &0x9ABCu16.to_le_bytes());
        assert_eq!(bytes[18], 2); // allowed_widths length
        assert_eq!(&bytes[19..21], &1920u16.to_le_bytes());
        assert_eq!(&bytes[21..23], &1280u16.to_le_bytes());
    }

    #[test]
    fn test_claim_manager_from_config_with_keys() {
        let config_keys = vec![(1, [1u8; 32]), (3, [2u8; 32])];

        // Create manager with configured keys
        let manager = ClaimManager::from_config(config_keys).unwrap();

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
        let verified = manager.verify_claim(&token).unwrap();

        assert_eq!(verified.asset_id, payload.asset_id);
        assert_eq!(verified.exp_unix, payload.exp_unix);
    }

    #[test]
    fn test_claim_manager_from_config_without_keys() {
        // Test fallback to random generation
        let manager = ClaimManager::from_config(Vec::new()).unwrap();

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
        let verified = manager.verify_claim(&token).unwrap();

        assert_eq!(verified.asset_id, payload.asset_id);
    }

    #[test]
    fn test_claim_manager_from_config_empty_keys() {
        let config_keys = Vec::new();

        let result = ClaimManager::from_config(config_keys);
        assert!(result.is_ok());
        let key = result.unwrap();
        assert!(key.keys.read().len() == 1);
    }

    #[test]
    fn test_claim_manager_key_rotation() {
        let config_keys = vec![(1, [1u8; 32]), (2, [2u8; 32])];

        let manager = ClaimManager::from_config(config_keys).unwrap();

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
        let verified1 = manager.verify_claim(&token1).unwrap();
        let verified2 = manager.verify_claim(&token2).unwrap();

        assert_eq!(verified1.asset_id, payload.asset_id);
        assert_eq!(verified2.asset_id, payload.asset_id);

        // Tokens should be different (different keys used)
        assert_ne!(token1, token2);
    }
}
