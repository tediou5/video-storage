use crate::claim::error::ClaimError;
use crate::claim::payload_v1::ClaimPayloadV1;
use std::time::{SystemTime, UNIX_EPOCH};

/// HLS segment duration in seconds (from utils.rs)
pub const HLS_SEGMENT_DURATION: u16 = 4;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn test_claim_validation_with_zero_values() {
        // Test with zero window_len_sec (unlimited)
        let claim_unlimited_window = ClaimPayloadV1 {
            exp_unix: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 3600) as u32,
            nbf_unix: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 60) as u32,
            asset_id: "test_video".to_string(),
            window_len_sec: 0, // 0 = unlimited
            max_kbps: 5000,
            max_concurrency: 10,
            allowed_widths: vec![1920],
        };

        // Should allow any segment index when window_len_sec is 0
        assert!(
            validate_claim_time_and_resource(&claim_unlimited_window, "test_video", Some(0), None)
                .is_ok()
        );
        assert!(
            validate_claim_time_and_resource(
                &claim_unlimited_window,
                "test_video",
                Some(100),
                None
            )
            .is_ok()
        );
        assert!(
            validate_claim_time_and_resource(
                &claim_unlimited_window,
                "test_video",
                Some(10000),
                None
            )
            .is_ok()
        );

        // Test with limited window
        let claim_limited_window = ClaimPayloadV1 {
            exp_unix: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 3600) as u32,
            nbf_unix: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 60) as u32,
            asset_id: "test_video".to_string(),
            window_len_sec: 120, // 30 segments at 4 seconds each
            max_kbps: 5000,
            max_concurrency: 10,
            allowed_widths: vec![1920],
        };

        // Should allow segments within the window
        assert!(
            validate_claim_time_and_resource(&claim_limited_window, "test_video", Some(0), None)
                .is_ok()
        );
        assert!(
            validate_claim_time_and_resource(&claim_limited_window, "test_video", Some(29), None)
                .is_ok()
        );
        assert!(
            validate_claim_time_and_resource(&claim_limited_window, "test_video", Some(30), None)
                .is_ok()
        );
        // Should deny segments outside the window
        assert!(
            validate_claim_time_and_resource(&claim_limited_window, "test_video", Some(31), None)
                .is_err()
        );
    }

    #[test]
    fn test_claim_validation_with_empty_allowed_widths() {
        // Test with empty allowed_widths (all widths allowed)
        let claim_all_widths = ClaimPayloadV1 {
            exp_unix: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 3600) as u32,
            nbf_unix: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 60) as u32,
            asset_id: "test_video".to_string(),
            window_len_sec: 0,
            max_kbps: 0,
            max_concurrency: 0,
            allowed_widths: vec![], // Empty = all widths allowed
        };

        // Should allow any width when allowed_widths is empty
        assert!(
            validate_claim_time_and_resource(&claim_all_widths, "test_video", None, Some(1920))
                .is_ok()
        );
        assert!(
            validate_claim_time_and_resource(&claim_all_widths, "test_video", None, Some(1280))
                .is_ok()
        );
        assert!(
            validate_claim_time_and_resource(&claim_all_widths, "test_video", None, Some(854))
                .is_ok()
        );
        assert!(
            validate_claim_time_and_resource(&claim_all_widths, "test_video", None, Some(640))
                .is_ok()
        );
        assert!(
            validate_claim_time_and_resource(&claim_all_widths, "test_video", None, Some(480))
                .is_ok()
        );
        assert!(
            validate_claim_time_and_resource(&claim_all_widths, "test_video", None, None).is_ok()
        );

        // Test with specific allowed widths
        let claim_limited_widths = ClaimPayloadV1 {
            exp_unix: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 3600) as u32,
            nbf_unix: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 60) as u32,
            asset_id: "test_video".to_string(),
            window_len_sec: 0,
            max_kbps: 0,
            max_concurrency: 0,
            allowed_widths: vec![1920, 1280], // Only allow specific widths
        };

        // Should allow listed widths
        assert!(
            validate_claim_time_and_resource(&claim_limited_widths, "test_video", None, Some(1920))
                .is_ok()
        );
        assert!(
            validate_claim_time_and_resource(&claim_limited_widths, "test_video", None, Some(1280))
                .is_ok()
        );
        // Should deny unlisted widths
        assert!(
            validate_claim_time_and_resource(&claim_limited_widths, "test_video", None, Some(854))
                .is_err()
        );
        assert!(
            validate_claim_time_and_resource(&claim_limited_widths, "test_video", None, Some(640))
                .is_err()
        );
        // No width specified should be ok
        assert!(
            validate_claim_time_and_resource(&claim_limited_widths, "test_video", None, None)
                .is_ok()
        );
    }

    #[test]
    fn test_claim_payload_with_all_zeros() {
        // Test a claim with all limit values set to 0 (meaning unlimited)
        let claim = ClaimPayloadV1 {
            exp_unix: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 3600) as u32,
            nbf_unix: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 60) as u32,
            asset_id: "unlimited_video".to_string(),
            window_len_sec: 0,      // Unlimited time window
            max_kbps: 0,            // Unlimited bandwidth
            max_concurrency: 0,     // Unlimited connections
            allowed_widths: vec![], // All widths allowed
        };

        // Should allow everything
        assert!(
            validate_claim_time_and_resource(&claim, "unlimited_video", Some(9999), Some(3840))
                .is_ok()
        );
        assert!(validate_claim_time_and_resource(&claim, "unlimited_video", None, None).is_ok());

        // Should still validate asset_id
        assert!(validate_claim_time_and_resource(&claim, "wrong_video", None, None).is_err());

        // Should still validate time bounds
        let expired_claim = ClaimPayloadV1 {
            exp_unix: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 3600) as u32,
            nbf_unix: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 7200) as u32,
            asset_id: "unlimited_video".to_string(),
            window_len_sec: 0,
            max_kbps: 0,
            max_concurrency: 0,
            allowed_widths: vec![],
        };
        assert!(
            validate_claim_time_and_resource(&expired_claim, "unlimited_video", None, None)
                .is_err()
        );
    }

    #[test]
    fn test_claim_validation() {
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
}
