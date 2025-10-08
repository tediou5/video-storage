pub mod payload_v1;
pub mod payload_v2;

use std::time::{SystemTime, UNIX_EPOCH};

pub use payload_v1::ClaimPayloadV1;
pub use payload_v2::ClaimPayloadV2;

use crate::{ClaimError, HLS_SEGMENT_DURATION};

pub trait Payload: Sized {
    fn version(&self) -> u8;
    fn verify_asset_id(&self, asset_id: &str) -> bool;
    fn nbf_unix(&self) -> u32;
    fn exp_unix(&self) -> u32;
    fn window_len_sec(&self) -> u16;
    fn max_kbps(&self) -> u16;
    fn max_concurrency(&self) -> u16;
    fn allowed_widths(&self) -> &[u16];

    fn verify(
        data: &[u8],
        asset_id: &str,
        segment_index: Option<u32>,
        width: Option<u16>,
    ) -> Result<Self, ClaimError> {
        let payload: Self = Payload::deserialize(data)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;

        // Check time bounds
        if now < payload.nbf_unix() {
            return Err(ClaimError::TokenNotYetValid);
        }
        if now >= payload.exp_unix() {
            return Err(ClaimError::TokenExpired);
        }

        // Check asset ID
        if !payload.verify_asset_id(asset_id) {
            return Err(ClaimError::AssetMismatch);
        }

        // Check time window for segments
        if let Some(seg_idx) = segment_index
            && payload.window_len_sec() != 0
        {
            let max_segment = payload.window_len_sec() / HLS_SEGMENT_DURATION;
            // Allow access to the segment if it's within or partially within the window
            if seg_idx > max_segment as u32 {
                return Err(ClaimError::TimeWindowDeny);
            }
        }

        // Check width restrictions if specified
        if let Some(requested_width) = width {
            let allowed_widths = payload.allowed_widths();
            // If allowed_widths is not empty, check if the requested width is allowed
            if !allowed_widths.is_empty() && !allowed_widths.contains(&requested_width) {
                return Err(ClaimError::AssetMismatch);
            }
        }

        Ok(payload)
    }

    fn serialize(&self) -> anyhow::Result<Vec<u8>>;
    fn deserialize(data: &[u8]) -> Result<Self, ClaimError>;
}
