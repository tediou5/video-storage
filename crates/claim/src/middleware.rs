use crate::{ClaimManager, validate_claim_time_and_resource};
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tracing::{debug, warn};

/// Create an error response
fn err_response(status: StatusCode, message: &str) -> Response {
    let body = json!({
        "error": message,
        "status": status.as_u16()
    });

    (status, body.to_string()).into_response()
}

/// Claim verification state to be passed to handlers
#[derive(Clone)]
pub struct ClaimState {
    pub claim_manager: ClaimManager,
}

/// Middleware for claim-based authentication and authorization
pub async fn claim_auth_middleware(
    State(claim_state): State<ClaimState>,
    Path(filename): Path<String>,
    mut req: Request,
    next: Next,
) -> Response {
    // Extract Authorization header and convert to owned String early
    let token = {
        let auth_header = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok());

        match auth_header {
            Some(header) if header.starts_with("Bearer ") => header[7..].to_string(),
            _ => {
                warn!("Missing or invalid Authorization header");
                return err_response(
                    StatusCode::UNAUTHORIZED,
                    "Missing or invalid Authorization header",
                );
            }
        }
    };

    // Verify the claim token
    let (claim_payload, bucket_key) = match claim_state.claim_manager.verify_claim(&token) {
        Ok((payload, key)) => (payload, key),
        Err(error) => {
            warn!(?error, "Failed to verify claim");
            return (error.to_err_code(), error.to_string()).into_response();
        }
    };

    // Parse filename to extract asset_id, segment info, and width
    let (asset_id, segment_index, width) = match parse_video_filename(&filename) {
        Some(info) => info,
        None => {
            warn!("Invalid filename format: {}", filename);
            return err_response(StatusCode::BAD_REQUEST, "Invalid filename format");
        }
    };

    // Validate claim against resource and time window
    if let Err(error) =
        validate_claim_time_and_resource(&claim_payload, &asset_id, segment_index, width)
    {
        let status = error.to_err_code();

        warn!(
            asset_id,
            ?segment_index,
            %error,
            "Claim validation failed"
        );

        return (status, error.to_string()).into_response();
    }

    debug!(
        asset_id,
        ?segment_index,
        max_kbps = claim_payload.max_kbps,
        window_sec = claim_payload.window_len_sec,
        "Claim validated successfully"
    );

    // Get or create rate limit bucket for this claim
    let bucket = claim_state
        .claim_manager
        .get_or_create_bucket(&bucket_key, &claim_payload);

    // Try to acquire a connection slot for concurrency control
    let Ok(_guard) = bucket.try_acquire_connection() else {
        warn!(
            asset_id,
            max_concurrency = claim_payload.max_concurrency,
            "Max concurrency exceeded"
        );

        return err_response(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many concurrent connections",
        );
    };

    // Store bucket in extensions for the route handler to use
    req.extensions_mut().insert(bucket);

    // Update last access time
    claim_state.claim_manager.touch_bucket(&bucket_key);

    // Continue to the actual handler
    next.run(req).await
}

/// Parse video filename to extract asset_id and optional segment index
/// Supports formats:
/// - {asset_id}.m3u8 (master playlist)
/// - {width}/{asset_id}.m3u8 (resolution playlist)
/// - {width}/{asset_id}-{segment}.m4s (video segment)
/// - {width}/{asset_id}-init.mp4 (init segment)
fn parse_video_filename(filename: &str) -> Option<(String, Option<u32>, Option<u16>)> {
    // Split by path separator first
    let parts: Vec<&str> = filename.split('/').collect();

    let (file_part, width) = if parts.len() == 2 {
        // Format: {width}/{filename}
        let width = parts[0].parse::<u16>().ok();
        (parts[1], width)
    } else {
        // Format: {filename}
        (parts[0], None)
    };

    // Check for master playlist
    if file_part.ends_with(".m3u8") {
        let asset_id = file_part.trim_end_matches(".m3u8");
        return Some((asset_id.to_string(), None, width));
    }

    // Check for init segment
    if file_part.ends_with("-init.mp4") {
        let asset_id = file_part.trim_end_matches("-init.mp4");
        return Some((asset_id.to_string(), Some(0), width)); // Init segment gets index 0
    }

    // Check for video segment
    if file_part.contains('-') && file_part.ends_with(".m4s") {
        let without_ext = file_part.trim_end_matches(".m4s");
        let parts: Vec<&str> = without_ext.rsplitn(2, '-').collect();

        if parts.len() == 2 {
            // parts[0] is segment number (reversed), parts[1] is asset_id
            if let Ok(segment_num) = parts[0].parse::<u32>() {
                return Some((parts[1].to_string(), Some(segment_num), width));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_video_filename() {
        // Master playlist
        let (asset_id, seg, width) = parse_video_filename("video123.m3u8").unwrap();
        assert_eq!(asset_id, "video123");
        assert_eq!(seg, None);
        assert_eq!(width, None);

        // Resolution playlist
        let (asset_id, seg, width) = parse_video_filename("720/video123.m3u8").unwrap();
        assert_eq!(asset_id, "video123");
        assert_eq!(seg, None);
        assert_eq!(width, Some(720));

        // Init segment
        let (asset_id, seg, width) = parse_video_filename("720/video123-init.mp4").unwrap();
        assert_eq!(asset_id, "video123");
        assert_eq!(seg, Some(0));
        assert_eq!(width, Some(720));

        // Video segment
        let (asset_id, seg, width) = parse_video_filename("720/video123-005.m4s").unwrap();
        assert_eq!(asset_id, "video123");
        assert_eq!(seg, Some(5));
        assert_eq!(width, Some(720));

        // Invalid format
        assert!(parse_video_filename("invalid.txt").is_none());
    }
}
