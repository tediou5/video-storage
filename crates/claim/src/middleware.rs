use crate::ClaimManager;
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tracing::warn;

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

    // Parse filename to extract asset_id, segment info, and width
    let (asset_id, segment_index, width) = match parse_video_filename(&filename) {
        Some(info) => info,
        None => {
            warn!("Invalid filename format: {}", filename);
            return err_response(StatusCode::BAD_REQUEST, "Invalid filename format");
        }
    };

    // Verify the claim token
    let (bucket, bucket_key) =
        match claim_state
            .claim_manager
            .verify_claim(&token, &asset_id, segment_index, width)
        {
            Ok(result) => result,
            Err(err) => {
                warn!("Failed to verify claim: {}", err);
                return err_response(err.to_err_code(), &err.to_string());
            }
        };

    // Try to acquire a connection slot for concurrency control
    let Ok(_guard) = bucket.try_acquire_connection() else {
        warn!(
            asset_id,
            max_concurrency = bucket.max_concurrency,
            "Max concurrency exceeded"
        );

        return err_response(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many concurrent connections",
        );
    };
    claim_state.claim_manager.touch_bucket(&bucket_key);

    req.extensions_mut().insert(bucket);
    next.run(req).await
}

/// Remove codec suffix from asset_id if present
fn remove_codec_suffix(asset_id: &str) -> &str {
    // Remove codec suffix
    asset_id
        .strip_suffix("-h265")
        .or_else(|| asset_id.strip_suffix("-h264"))
        .unwrap_or(asset_id)
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
        let asset_id_with_suffix = file_part.trim_end_matches(".m3u8");
        let asset_id = remove_codec_suffix(asset_id_with_suffix);
        return Some((asset_id.to_string(), None, width));
    }

    // Check for init segment
    if file_part.ends_with("-init.mp4") {
        let asset_id_with_suffix = file_part.trim_end_matches("-init.mp4");
        let asset_id = remove_codec_suffix(asset_id_with_suffix);
        return Some((asset_id.to_string(), Some(0), width)); // Init segment gets index 0
    }

    // Check for video segment
    if file_part.contains('-') && file_part.ends_with(".m4s") {
        let without_ext = file_part.trim_end_matches(".m4s");
        let parts: Vec<&str> = without_ext.rsplitn(2, '-').collect();

        if parts.len() == 2 {
            // parts[0] is segment number (reversed), parts[1] is asset_id_with_suffix
            if let Ok(segment_num) = parts[0].parse::<u32>() {
                let asset_id = remove_codec_suffix(parts[1]);
                return Some((asset_id.to_string(), Some(segment_num), width));
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

        // H265 master playlist
        let (asset_id, seg, width) = parse_video_filename("video123-h265.m3u8").unwrap();
        assert_eq!(asset_id, "video123");
        assert_eq!(seg, None);
        assert_eq!(width, None);

        // H265 resolution playlist
        let (asset_id, seg, width) = parse_video_filename("720/video123-h265.m3u8").unwrap();
        assert_eq!(asset_id, "video123");
        assert_eq!(seg, None);
        assert_eq!(width, Some(720));

        // H265 init segment
        let (asset_id, seg, width) = parse_video_filename("720/video123-h265-init.mp4").unwrap();
        assert_eq!(asset_id, "video123");
        assert_eq!(seg, Some(0));
        assert_eq!(width, Some(720));

        // H265 video segment
        let (asset_id, seg, width) = parse_video_filename("720/video123-h265-005.m4s").unwrap();
        assert_eq!(asset_id, "video123");
        assert_eq!(seg, Some(5));
        assert_eq!(width, Some(720));

        // Invalid format
        assert!(parse_video_filename("invalid.txt").is_none());
    }
}
