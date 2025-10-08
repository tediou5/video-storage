use crate::{error::ClaimError, payload::Payload};
use anyhow::{Result, anyhow};
use xorf::BinaryFuse16;

#[derive(Debug, Clone)]
pub struct ClaimPayloadV2 {
    /// Expiration time in Unix timestamp
    pub exp_unix: u32,
    /// Not-before time in Unix timestamp
    pub nbf_unix: u32,
    /// Assets filter map (job_ids) that this claim grants access to
    pub assets_filter: BinaryFuse16,
    /// Time window length in seconds (0 = unlimited)
    pub window_len_sec: u16,
    /// Maximum bandwidth in kbps (0 = unlimited)
    pub max_kbps: u16,
    /// Maximum concurrent connections (0 = unlimited)
    pub max_concurrency: u16,
    /// Allowed video widths/resolutions (empty = all widths allowed)
    pub allowed_widths: Vec<u16>,
}

impl Payload for ClaimPayloadV2 {
    fn version(&self) -> u8 {
        2
    }

    fn serialize(&self) -> Result<Vec<u8>> {
        self.serialize_to_bytes()
    }

    fn deserialize(data: &[u8]) -> Result<Self, ClaimError> {
        ClaimPayloadV2::deserialize_from_bytes(data)
    }

    fn verify_asset_id(&self, asset_id: &str) -> bool {
        self.assets_filter.contains(&asset_id)
    }

    fn nbf_unix(&self) -> u32 {
        self.nbf_unix
    }

    fn exp_unix(&self) -> u32 {
        self.exp_unix
    }

    fn window_len_sec(&self) -> u16 {
        self.window_len_sec
    }

    fn max_kbps(&self) -> u16 {
        self.max_kbps
    }

    fn max_concurrency(&self) -> u16 {
        self.max_concurrency
    }

    fn allowed_widths(&self) -> &[u16] {
        self.allowed_widths.as_slice()
    }
}

impl ClaimPayloadV2 {
    /// Serialize payload to binary format
    pub fn serialize_to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();

        // exp_unix (4 bytes)
        bytes.extend_from_slice(&self.exp_unix.to_le_bytes());

        // nbf_unix (4 bytes)
        bytes.extend_from_slice(&self.nbf_unix.to_le_bytes());

        // assets_filter (variable length)
        let filter_bytes = self.assets_filter.serialize();
        bytes.extend_from_slice(&filter_bytes);

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
    pub fn deserialize_from_bytes(bytes: &[u8]) -> Result<ClaimPayloadV2, ClaimError> {
        if bytes.len() < 10 {
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

        // assets_filter (variable length)
        // Peek the next 2 bytes to get filter len
        if bytes.len() < offset + 2 {
            return Err(ClaimError::InvalidPayload(
                "Missing filter length".to_string(),
            ));
        }
        let filter_len = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        let filter_total_len = 2 + xorf::Descriptor::LEN + (filter_len as usize * 2);

        if bytes.len() < offset + filter_total_len {
            return Err(ClaimError::InvalidPayload(
                "Invalid filter size".to_string(),
            ));
        }

        let assets_filter = BinaryFuse16::deserialize(&bytes[offset..offset + filter_total_len])
            .map_err(|e| {
                ClaimError::InvalidPayload(format!("Failed to deserialize filter: {}", e))
            })?;
        offset += filter_total_len;

        // Check if we have enough bytes for remaining fields
        if bytes.len() < offset + 6 {
            return Err(ClaimError::InvalidPayload(
                "Invalid payload size for remaining fields".to_string(),
            ));
        }

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

        Ok(ClaimPayloadV2 {
            exp_unix,
            nbf_unix,
            assets_filter,
            window_len_sec,
            max_kbps,
            max_concurrency,
            allowed_widths,
        })
    }
}

#[allow(dead_code)]
fn new_filter(assets: &[String]) -> anyhow::Result<BinaryFuse16> {
    BinaryFuse16::try_from(assets)
        .map_err(|error| anyhow::anyhow!("Failed to create xorf filter: {error}",))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_claim_v2_serialization_deserialization() {
        // Create a test filter
        let assets = vec![
            "test1".to_string(),
            "test2".to_string(),
            "test3".to_string(),
        ];
        let assets_filter = BinaryFuse16::try_from(&assets).unwrap();

        let payload = ClaimPayloadV2 {
            exp_unix: 2000000000,
            nbf_unix: 1700000000,
            assets_filter,
            window_len_sec: 180,
            max_kbps: 4000,
            max_concurrency: 10,
            allowed_widths: vec![1920, 1280, 854],
        };

        // Test serialization and deserialization
        let serialized = payload.serialize_to_bytes().unwrap();
        let deserialized = ClaimPayloadV2::deserialize_from_bytes(&serialized).unwrap();

        assert_eq!(deserialized.exp_unix, payload.exp_unix);
        assert_eq!(deserialized.nbf_unix, payload.nbf_unix);
        assert_eq!(deserialized.window_len_sec, payload.window_len_sec);
        assert_eq!(deserialized.max_kbps, payload.max_kbps);
        assert_eq!(deserialized.max_concurrency, payload.max_concurrency);
        assert_eq!(deserialized.allowed_widths, payload.allowed_widths);

        // Test that the filter works correctly
        for asset in &assets {
            assert!(deserialized.assets_filter.contains(asset));
        }
    }

    #[tokio::test]
    async fn test_payload_trait() {
        let assets = vec!["asset123".to_string()];
        let assets_filter = BinaryFuse16::try_from(&assets).unwrap();

        let payload = ClaimPayloadV2 {
            exp_unix: 2000000000,
            nbf_unix: 1700000000,
            assets_filter,
            window_len_sec: 300,
            max_kbps: 8000,
            max_concurrency: 5,
            allowed_widths: vec![1920, 1280],
        };

        // Test Payload trait methods
        assert_eq!(payload.version(), 2);
        assert!(payload.verify_asset_id("asset123"));
        assert!(!payload.verify_asset_id("nonexistent"));
        assert_eq!(payload.nbf_unix(), 1700000000);
        assert_eq!(payload.exp_unix(), 2000000000);
        assert_eq!(payload.window_len_sec(), 300);
        assert_eq!(payload.max_kbps(), 8000);
        assert_eq!(payload.max_concurrency(), 5);
        assert_eq!(payload.allowed_widths(), &[1920, 1280]);
    }
}
