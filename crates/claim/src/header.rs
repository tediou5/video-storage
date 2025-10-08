use crate::error::ClaimError;
use std::time::{SystemTime, UNIX_EPOCH};

// Constants for token format
pub(crate) const MAGIC: &[u8; 4] = b"VSC1";
pub(crate) const VERSION: u8 = 1;
pub(crate) const ALG_AES_256_GCM: u8 = 1;
#[allow(dead_code)]
pub(crate) const ALG_CHACHA20_POLY1305: u8 = 2;

// Header size: magic(4) + ver(1) + kid(1) + alg(1) + rsv(1) + nonce(12) + create_at(16) = 36 bytes
pub(crate) const HEADER_SIZE: usize = 36;
pub(crate) const TAG_SIZE: usize = 16;

/// Binary header structure (plaintext)
#[derive(Debug, Clone)]
pub struct ClaimHeader {
    pub magic: [u8; 4],
    pub version: u8,
    pub kid: u8,
    pub alg: u8,
    pub rsv: u8,
    pub nonce: [u8; 12],
    // Creation time in nanoseconds since UNIX_EPOCH
    pub create_at: u128,
}

impl ClaimHeader {
    pub fn new(kid: u8, alg: u8) -> Self {
        let mut nonce = [0u8; 12];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut nonce);

        let create_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        Self {
            magic: *MAGIC,
            version: VERSION,
            kid,
            alg,
            rsv: 0,
            nonce,
            create_at,
        }
    }

    /// Generate bucket key from nonce and create_at
    pub fn bucket_key(&self) -> [u8; 28] {
        let mut key_bytes = [0u8; 28];
        key_bytes[0..12].copy_from_slice(&self.nonce);
        key_bytes[12..28].copy_from_slice(&self.create_at.to_le_bytes());
        key_bytes
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_SIZE);
        bytes.extend_from_slice(&self.magic);
        bytes.push(self.version);
        bytes.push(self.kid);
        bytes.push(self.alg);
        bytes.push(self.rsv);
        bytes.extend_from_slice(&self.nonce);
        bytes.extend_from_slice(&self.create_at.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ClaimError> {
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

        let create_at_bytes: [u8; 16] = bytes[20..36]
            .try_into()
            .map_err(|_| ClaimError::InvalidHeader("Failed to read create_at".to_string()))?;
        let create_at = u128::from_le_bytes(create_at_bytes);

        Ok(Self {
            magic,
            version,
            kid,
            alg,
            rsv,
            nonce,
            create_at,
        })
    }
}
