use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Key, Nonce};
use anyhow::{Result, anyhow};
use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::RwLock;

// Constants for token format
const MAGIC: &[u8; 4] = b"VSC1";
const VERSION: u8 = 1;
const ALG_AES_256_GCM: u8 = 1;
#[allow(dead_code)]
const ALG_CHACHA20_POLY1305: u8 = 2;

// Header size: magic(4) + ver(1) + kid(1) + alg(1) + rsv(1) + nonce(12) = 20 bytes
const HEADER_SIZE: usize = 20;
const TAG_SIZE: usize = 16;

/// HLS segment duration in seconds (from utils.rs)
pub const HLS_SEGMENT_DURATION: u16 = 4;

/// Claim-related errors with API error codes
#[derive(Debug, Error)]
pub enum ClaimError {
    #[error("Invalid token format")]
    InvalidToken,

    #[error("Token has expired")]
    TokenExpired,

    #[error("Token is not yet valid")]
    TokenNotYetValid,

    #[error("AEAD decryption failed")]
    AeadFail,

    #[error("Asset ID mismatch")]
    AssetMismatch,

    #[error("Time window exceeded")]
    TimeWindowDeny,

    #[error("Key not found: {0}")]
    KeyNotFound(u8),

    #[error("Invalid header: {0}")]
    InvalidHeader(String),

    #[error("Invalid payload: {0}")]
    InvalidPayload(String),
}

impl ClaimError {
    /// Convert error to HTTP status code and API error code string
    pub fn to_err_code(&self) -> StatusCode {
        match self {
            ClaimError::InvalidToken
            | ClaimError::TokenExpired
            | ClaimError::TokenNotYetValid
            | ClaimError::AeadFail
            | ClaimError::KeyNotFound(_)
            | ClaimError::InvalidHeader(_)
            | ClaimError::InvalidPayload(_) => StatusCode::UNAUTHORIZED,
            ClaimError::AssetMismatch | ClaimError::TimeWindowDeny => StatusCode::FORBIDDEN,
        }
    }
}

/// Claim payload structure (to be encrypted)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimPayloadV1 {
    pub exp_unix: u32,
    pub nbf_unix: u32,
    pub asset_id: String,
    pub window_len_sec: u16,
    pub max_kbps: u16,
    pub max_concurrency: u16,
    pub allowed_widths: Vec<u16>,
}

/// Binary header structure (plaintext)
#[derive(Debug, Clone)]
struct ClaimHeader {
    magic: [u8; 4],
    version: u8,
    kid: u8,
    alg: u8,
    rsv: u8,
    nonce: [u8; 12],
}

impl ClaimHeader {
    fn new(kid: u8, alg: u8) -> Self {
        let mut nonce = [0u8; 12];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut nonce);

        Self {
            magic: *MAGIC,
            version: VERSION,
            kid,
            alg,
            rsv: 0,
            nonce,
        }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_SIZE);
        bytes.extend_from_slice(&self.magic);
        bytes.push(self.version);
        bytes.push(self.kid);
        bytes.push(self.alg);
        bytes.push(self.rsv);
        bytes.extend_from_slice(&self.nonce);
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, ClaimError> {
        if bytes.len() < HEADER_SIZE {
            return Err(ClaimError::InvalidHeader("Invalid header size".to_string()));
        }

        let magic: [u8; 4] = bytes[0..4]
            .try_into()
            .map_err(|_| ClaimError::InvalidHeader("Failed to read magic bytes".to_string()))?;
        if magic != *MAGIC {
            return Err(ClaimError::InvalidHeader("Invalid magic bytes".to_string()));
        }

        let version = bytes[4];
        if version != VERSION {
            return Err(ClaimError::InvalidHeader(format!(
                "Unsupported version: {version}",
            )));
        }

        let kid = bytes[5];
        let alg = bytes[6];
        let rsv = bytes[7];
        let nonce: [u8; 12] = bytes[8..20]
            .try_into()
            .map_err(|_| ClaimError::InvalidHeader("Failed to read nonce".to_string()))?;

        Ok(Self {
            magic,
            version,
            kid,
            alg,
            rsv,
            nonce,
        })
    }
}

/// Claim manager for key management and token operations
#[derive(Clone, Debug)]
pub struct ClaimManager {
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
    pub fn from_config(config_keys: Option<HashMap<u8, [u8; 32]>>) -> Result<Self> {
        let Some(keys) = config_keys else {
            return Ok(Self::new());
        };

        if keys.is_empty() {
            return Ok(Self::new());
        }

        let first_kid = keys.keys().min().copied().expect("Must has key");

        Ok(Self {
            keys: Arc::new(RwLock::new(keys)),
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
    pub async fn add_key(&self, kid: u8, key: [u8; 32]) -> Result<()> {
        let mut keys = self.keys.write().await;
        if keys.contains_key(&kid) {
            return Err(anyhow!("Key ID {} already exists", kid));
        }
        keys.insert(kid, key);
        Ok(())
    }

    /// Set the current key ID for signing new tokens
    #[allow(dead_code)]
    pub async fn set_current_kid(&self, kid: u8) -> Result<()> {
        let keys = self.keys.read().await;
        if !keys.contains_key(&kid) {
            return Err(anyhow!("Key ID {} not found", kid));
        }
        self.current_kid.store(kid, Ordering::Relaxed);
        Ok(())
    }

    /// Sign a claim and return the base64url-encoded token
    pub async fn sign_claim(&self, payload: &ClaimPayloadV1) -> Result<String> {
        let kid = self.current_kid.load(Ordering::Relaxed);
        let keys = self.keys.read().await;
        let key = keys
            .get(&kid)
            .ok_or_else(|| anyhow!("Key ID {} not found", kid))?;

        // Create header
        let header = ClaimHeader::new(kid, ALG_AES_256_GCM);
        let header_bytes = header.to_bytes();

        // Serialize payload
        let payload_bytes = self.serialize_payload(payload)?;

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
            .map_err(|e| anyhow!("Encryption failed: {}", e))?;

        // Combine: header || ciphertext (includes tag)
        let mut token_bytes = header_bytes;
        token_bytes.extend_from_slice(&ciphertext);

        // Encode as base64url without padding
        Ok(URL_SAFE_NO_PAD.encode(token_bytes))
    }

    /// Verify and decode a claim token
    pub async fn verify_claim(&self, token: &str) -> Result<ClaimPayloadV1, ClaimError> {
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
        let keys = self.keys.read().await;
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
        self.deserialize_payload(&payload_bytes)
    }

    /// Serialize payload to binary format
    fn serialize_payload(&self, payload: &ClaimPayloadV1) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();

        // exp_unix (4 bytes)
        bytes.extend_from_slice(&payload.exp_unix.to_le_bytes());

        // nbf_unix (4 bytes)
        bytes.extend_from_slice(&payload.nbf_unix.to_le_bytes());

        // id_len (1 byte)
        let id_bytes = payload.asset_id.as_bytes();
        if id_bytes.len() > 255 {
            return Err(anyhow!("Asset ID too long"));
        }
        bytes.push(id_bytes.len() as u8);

        // asset_id (variable)
        bytes.extend_from_slice(id_bytes);

        // window_len_sec (2 bytes)
        bytes.extend_from_slice(&payload.window_len_sec.to_le_bytes());

        // max_kbps (2 bytes)
        bytes.extend_from_slice(&payload.max_kbps.to_le_bytes());

        // max_concurrency (2 bytes)
        bytes.extend_from_slice(&payload.max_concurrency.to_le_bytes());

        // allowed_widths_len (1 byte)
        if payload.allowed_widths.len() > 255 {
            return Err(anyhow!("Too many allowed widths"));
        }
        bytes.push(payload.allowed_widths.len() as u8);

        // allowed_widths (2 bytes each)
        for width in &payload.allowed_widths {
            bytes.extend_from_slice(&width.to_le_bytes());
        }

        Ok(bytes)
    }

    /// Deserialize payload from binary format
    fn deserialize_payload(&self, bytes: &[u8]) -> Result<ClaimPayloadV1, ClaimError> {
        if bytes.len() < 13 {
            return Err(ClaimError::InvalidPayload("Payload too short".to_string()));
        }

        let mut offset = 0;

        // exp_unix (4 bytes)
        let exp_unix = u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| ClaimError::InvalidPayload("Failed to read exp_unix".to_string()))?,
        );
        offset += 4;

        // nbf_unix (4 bytes)
        let nbf_unix = u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| ClaimError::InvalidPayload("Failed to read nbf_unix".to_string()))?,
        );
        offset += 4;

        // id_len (1 byte)
        let id_len = bytes[offset] as usize;
        offset += 1;

        // Check if we have enough bytes for asset_id and remaining fields
        if bytes.len() < offset + id_len + 6 {
            return Err(ClaimError::InvalidPayload(
                "Invalid payload size".to_string(),
            ));
        }

        // asset_id (variable)
        let asset_id = String::from_utf8(bytes[offset..offset + id_len].to_vec())
            .map_err(|_| ClaimError::InvalidPayload("Invalid UTF-8 in asset_id".to_string()))?;
        offset += id_len;

        // window_len_sec (2 bytes)
        let window_len_sec =
            u16::from_le_bytes(bytes[offset..offset + 2].try_into().map_err(|_| {
                ClaimError::InvalidPayload("Failed to read window_len_sec".to_string())
            })?);
        offset += 2;

        // max_kbps (2 bytes)
        let max_kbps = u16::from_le_bytes(
            bytes[offset..offset + 2]
                .try_into()
                .map_err(|_| ClaimError::InvalidPayload("Failed to read max_kbps".to_string()))?,
        );
        offset += 2;

        // max_concurrency (2 bytes)
        let max_concurrency =
            u16::from_le_bytes(bytes[offset..offset + 2].try_into().map_err(|_| {
                ClaimError::InvalidPayload("Failed to read max_concurrency".to_string())
            })?);
        offset += 2;

        // allowed_widths_len (1 byte)
        if offset >= bytes.len() {
            return Err(ClaimError::InvalidPayload(
                "Missing allowed_widths data".to_string(),
            ));
        }
        let allowed_widths_len = bytes[offset] as usize;
        offset += 1;

        // Check if we have enough bytes for allowed_widths
        if bytes.len() < offset + allowed_widths_len * 2 {
            return Err(ClaimError::InvalidPayload(
                "Invalid allowed_widths size".to_string(),
            ));
        }

        // allowed_widths (2 bytes each)
        let mut allowed_widths = Vec::with_capacity(allowed_widths_len);
        for _ in 0..allowed_widths_len {
            let width = u16::from_le_bytes(
                bytes[offset..offset + 2]
                    .try_into()
                    .map_err(|_| ClaimError::InvalidPayload("Failed to read width".to_string()))?,
            );
            allowed_widths.push(width);
            offset += 2;
        }

        Ok(ClaimPayloadV1 {
            exp_unix,
            nbf_unix,
            asset_id,
            window_len_sec,
            max_kbps,
            max_concurrency,
            allowed_widths,
        })
    }
}

impl Default for ClaimManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Validate claim against current time and resource
pub fn validate_claim_time_and_resource(
    claim: &ClaimPayloadV1,
    asset_id: &str,
    segment_index: Option<u32>,
    width: Option<u16>,
) -> Result<(), ClaimError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;

    // Check time bounds
    if now < claim.nbf_unix {
        return Err(ClaimError::TokenNotYetValid);
    }
    if now >= claim.exp_unix {
        return Err(ClaimError::TokenExpired);
    }

    // Check asset ID
    if claim.asset_id != asset_id {
        return Err(ClaimError::AssetMismatch);
    }

    // Check time window for segments
    if let Some(seg_idx) = segment_index
        && claim.window_len_sec != 0
    {
        let max_segment = claim.window_len_sec / HLS_SEGMENT_DURATION;
        // Allow access to the segment if it's within or partially within the window
        if seg_idx > max_segment as u32 {
            return Err(ClaimError::TimeWindowDeny);
        }
    }

    // Check width restrictions if specified
    if let Some(requested_width) = width {
        // If allowed_widths is not empty, check if the requested width is allowed
        if !claim.allowed_widths.is_empty() && !claim.allowed_widths.contains(&requested_width) {
            return Err(ClaimError::AssetMismatch); // Using AssetMismatch for width denial
        }
    }

    Ok(())
}

/// Request structure for claim creation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateClaimRequest {
    pub asset_id: String,
    pub nbf_unix: Option<u32>,
    pub exp_unix: u32,
    pub window_len_sec: u16,
    pub max_kbps: u16,
    pub max_concurrency: u16,
    pub allowed_widths: Vec<u16>,
}

/// Response structure for claim creation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateClaimResponse {
    pub token: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_claim_sign_and_verify() {
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

        let token = manager.sign_claim(&payload).await.unwrap();
        assert!(!token.is_empty());

        let verified = manager.verify_claim(&token).await.unwrap();
        assert_eq!(verified.asset_id, payload.asset_id);
        assert_eq!(verified.exp_unix, payload.exp_unix);
        assert_eq!(verified.window_len_sec, payload.window_len_sec);
        assert_eq!(verified.max_kbps, payload.max_kbps);
        assert_eq!(verified.max_concurrency, payload.max_concurrency);
    }

    #[tokio::test]
    async fn test_claim_serialization_deserialization() {
        let manager = ClaimManager::new();

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
            let serialized = manager.serialize_payload(&payload).unwrap();
            let deserialized = manager.deserialize_payload(&serialized).unwrap();

            assert_eq!(deserialized.exp_unix, payload.exp_unix);
            assert_eq!(deserialized.nbf_unix, payload.nbf_unix);
            assert_eq!(deserialized.asset_id, payload.asset_id);
            assert_eq!(deserialized.window_len_sec, payload.window_len_sec);
            assert_eq!(deserialized.max_kbps, payload.max_kbps);
            assert_eq!(deserialized.max_concurrency, payload.max_concurrency);
        }
    }

    #[tokio::test]
    async fn test_claim_binary_format_compatibility() {
        let manager = ClaimManager::new();

        let payload = ClaimPayloadV1 {
            exp_unix: 0x12345678,
            nbf_unix: 0x87654321,
            asset_id: "abc".to_string(),
            window_len_sec: 0x1234,
            max_kbps: 0x5678,
            max_concurrency: 0x9ABC,
            allowed_widths: vec![1920, 1280],
        };

        let bytes = manager.serialize_payload(&payload).unwrap();

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

    #[tokio::test]
    async fn test_claim_validation() {
        let claim = ClaimPayloadV1 {
            exp_unix: 2000000000,
            nbf_unix: 1000000000,
            asset_id: "video123".to_string(),
            window_len_sec: 120,
            max_kbps: 3000,
            max_concurrency: 5,
            allowed_widths: vec![1920, 1280, 854],
        };

        // Test asset mismatch
        let result = validate_claim_time_and_resource(&claim, "wrong_id", None, None);
        assert!(result.is_err());

        // Test segment window
        let result = validate_claim_time_and_resource(&claim, "video123", Some(50), None);
        assert!(result.is_err()); // 50 * 4 = 200 seconds > 120 seconds window
    }

    #[tokio::test]
    async fn test_claim_manager_from_config_with_keys() {
        let mut config_keys = std::collections::HashMap::new();
        config_keys.insert(1, [1u8; 32]);
        config_keys.insert(3, [2u8; 32]);

        // Create manager with configured keys
        let manager = ClaimManager::from_config(Some(config_keys)).unwrap();

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

        let token = manager.sign_claim(&payload).await.unwrap();
        let verified = manager.verify_claim(&token).await.unwrap();

        assert_eq!(verified.asset_id, payload.asset_id);
        assert_eq!(verified.exp_unix, payload.exp_unix);
    }

    #[tokio::test]
    async fn test_claim_manager_from_config_without_keys() {
        // Test fallback to random generation
        let manager = ClaimManager::from_config(None).unwrap();

        let payload = ClaimPayloadV1 {
            exp_unix: 2000000000,
            nbf_unix: 1700000000,
            asset_id: "test_random".to_string(),
            window_len_sec: 100,
            max_kbps: 3000,
            max_concurrency: 5,
            allowed_widths: vec![],
        };

        let token = manager.sign_claim(&payload).await.unwrap();
        let verified = manager.verify_claim(&token).await.unwrap();

        assert_eq!(verified.asset_id, payload.asset_id);
    }

    #[tokio::test]
    async fn test_claim_manager_from_config_empty_keys() {
        let config_keys = std::collections::HashMap::new();

        let result = ClaimManager::from_config(Some(config_keys));
        assert!(result.is_ok());
        let key = result.unwrap();
        assert!(key.keys.read().await.len() == 1);
    }

    #[tokio::test]
    async fn test_claim_manager_key_rotation() {
        let mut config_keys = std::collections::HashMap::new();

        config_keys.insert(1, [1u8; 32]);
        config_keys.insert(2, [2u8; 32]);

        let manager = ClaimManager::from_config(Some(config_keys)).unwrap();

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

        let token1 = manager.sign_claim(&payload).await.unwrap();

        // Change current key to kid=2
        manager.set_current_kid(2).await.unwrap();
        let token2 = manager.sign_claim(&payload).await.unwrap();

        // Both tokens should be verifiable
        let verified1 = manager.verify_claim(&token1).await.unwrap();
        let verified2 = manager.verify_claim(&token2).await.unwrap();

        assert_eq!(verified1.asset_id, payload.asset_id);
        assert_eq!(verified2.asset_id, payload.asset_id);

        // Tokens should be different (different keys used)
        assert_ne!(token1, token2);
    }
}
