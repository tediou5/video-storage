use crate::error::ClaimError;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// Claim payload structure (to be encrypted)
/// Claim payload containing access restrictions and metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimPayloadV1 {
    /// Expiration time in Unix timestamp
    pub exp_unix: u32,
    /// Not-before time in Unix timestamp
    pub nbf_unix: u32,
    /// Asset ID (job_id) that this claim grants access to
    pub asset_id: String,
    /// Time window length in seconds (0 = unlimited)
    pub window_len_sec: u16,
    /// Maximum bandwidth in kbps (0 = unlimited)
    pub max_kbps: u16,
    /// Maximum concurrent connections (0 = unlimited)
    pub max_concurrency: u16,
    /// Allowed video widths/resolutions (empty = all widths allowed)
    pub allowed_widths: Vec<u16>,
}

impl ClaimPayloadV1 {
    /// Serialize payload to binary format
    pub fn serialize_to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();

        // exp_unix (4 bytes)
        bytes.extend_from_slice(&self.exp_unix.to_le_bytes());

        // nbf_unix (4 bytes)
        bytes.extend_from_slice(&self.nbf_unix.to_le_bytes());

        // id_len (1 byte)
        let id_bytes = self.asset_id.as_bytes();
        if id_bytes.len() > 255 {
            return Err(anyhow!("Asset ID too long"));
        }
        bytes.push(id_bytes.len() as u8);

        // asset_id (variable)
        bytes.extend_from_slice(id_bytes);

        // window_len_sec (2 bytes)
        bytes.extend_from_slice(&self.window_len_sec.to_le_bytes());

        // max_kbps (2 bytes)
        bytes.extend_from_slice(&self.max_kbps.to_le_bytes());

        // max_concurrency (2 bytes)
        bytes.extend_from_slice(&self.max_concurrency.to_le_bytes());

        // allowed_widths_len (1 byte)
        if self.allowed_widths.len() > 255 {
            return Err(anyhow!("Too many allowed widths"));
        }
        bytes.push(self.allowed_widths.len() as u8);

        // allowed_widths (2 bytes each)
        for width in &self.allowed_widths {
            bytes.extend_from_slice(&width.to_le_bytes());
        }

        Ok(bytes)
    }

    /// Deserialize payload from binary format
    pub fn deserialize_from_bytes(bytes: &[u8]) -> Result<ClaimPayloadV1, ClaimError> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
