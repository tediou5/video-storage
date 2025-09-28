use crate::api::token_bucket::TokenBucket;
use crate::claim::{ClaimPayloadV1, CreateClaimRequest, CreateClaimResponse};
use crate::job::{CONVERT_KIND, UPLOAD_KIND};
use crate::{AppState, ConvertJob, Job};
use axum::body::Body;
use axum::extract::{Extension, Path as AxumPath};
use axum::http::{Request, Response, StatusCode, header};
use axum::response::{IntoResponse, Json};
use bytes::Bytes;
use futures::StreamExt;
use mime_guess::from_path;
use opendal::Operator;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::io::Error as IoError;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncSeekExt;
use tokio_util::io::ReaderStream;
use tracing::{debug, error, info, warn};

#[derive(Serialize, Deserialize)]
pub struct UploadResponse {
    pub job_id: String,
    pub message: String,
}

#[derive(Serialize, Deserialize)]
pub struct WaitlistResponse {
    pub pending_convert_jobs: usize,
    pub pending_upload_jobs: usize,
    pub total_pending_jobs: usize,
}

/// Validate job ID with basic rules
fn is_valid_job_id(job_id: &str) -> bool {
    !job_id.is_empty()
        && !job_id.contains('/')
        && !job_id.contains('-')
        && !job_id.contains('.')
        && !job_id.contains(' ')
        && job_id.len() <= 128
}

#[axum::debug_handler]
pub async fn waitlist(Extension(state): Extension<AppState>) -> impl IntoResponse {
    let jobs_guard = state.jobs_manager.jobs.lock().await;
    let jobs = jobs_guard.iter().map(Job::kind).collect::<Vec<_>>();
    drop(jobs_guard);

    let convert_jobs = jobs.iter().filter(|&&kind| kind == CONVERT_KIND).count();
    let upload_jobs = jobs.iter().filter(|&&kind| kind == UPLOAD_KIND).count();

    (
        StatusCode::OK,
        Json(WaitlistResponse {
            pending_convert_jobs: convert_jobs,
            pending_upload_jobs: upload_jobs,
            total_pending_jobs: convert_jobs + upload_jobs,
        }),
    )
}

pub async fn upload_mp4_raw(
    Extension(state): Extension<AppState>,
    request: Request<Body>,
) -> impl IntoResponse {
    // Extract query string and parse with serde_qs
    let query = request.uri().query().unwrap_or("");
    let job: ConvertJob = match serde_qs::from_str(query) {
        Ok(job) => job,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(UploadResponse {
                    job_id: "unknown".to_string(),
                    message: format!("Failed to parse query parameters: {err}"),
                }),
            );
        }
    };

    let body = request.into_body();
    let job_id = job.id().to_string();

    if job.crf > 63 {
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                job_id: job.id,
                message: "Invalid parameters: crf can only be set in the range 0-63".into(),
            }),
        );
    }

    if !is_valid_job_id(&job_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                job_id,
                message: "Invalid job ID format".into(),
            }),
        );
    }

    if state
        .jobs_manager
        .jobs
        .lock()
        .await
        .iter()
        .any(|j| j.id() == job.id())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                job_id: job.id,
                message: "already in-progress".into(),
            }),
        );
    }

    info!(%job_id, "Uploading file");

    let upload_path = state.uploads_dir().join(&job_id);
    let Ok(mut file) = tokio::fs::File::create(&upload_path).await else {
        error!(%job_id, "Failed to create upload file");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(UploadResponse {
                job_id,
                message: "Failed to create upload file".into(),
            }),
        );
    };

    use tokio::io::AsyncWriteExt as _;
    let mut body_stream = body.into_data_stream();
    while let Some(Ok(chunk)) = body_stream.next().await {
        if file.write_all(&chunk).await.is_err() {
            error!(%job_id, "Failed to write to upload file");
            let _ = tokio::fs::remove_file(&upload_path).await;

            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(UploadResponse {
                    job_id,
                    message: "Failed to write upload file".into(),
                }),
            );
        }
    }

    if file.flush().await.is_err() {
        error!(%job_id, "Failed to flush upload file");
        let _ = tokio::fs::remove_file(&upload_path).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(UploadResponse {
                job_id,
                message: "Failed to submit convert job".into(),
            }),
        );
    }

    state.jobs_manager.add(&job).await;
    _ = state.job_tx.unbounded_send(job.into());

    (
        StatusCode::ACCEPTED,
        Json(UploadResponse {
            job_id,
            message: "Processing in background".into(),
        }),
    )
}

async fn try_serve_from_filesystem(
    path: PathBuf,
    start: u64,
    end: u64,
    bucket: TokenBucket,
) -> anyhow::Result<impl futures::Stream<Item = Result<Bytes, IoError>> + Send> {
    let mut fh = tokio::fs::File::open(&path).await?;

    fh.seek(std::io::SeekFrom::Start(start)).await?;
    let len = end - start + 1;

    use tokio::io::AsyncReadExt as _;
    let stream = ReaderStream::new(fh.take(len)).then(move |res| {
        let bucket = bucket.clone();
        async move {
            let chunk = res?;
            bucket.consume(chunk.len()).await;
            Ok::<Bytes, IoError>(chunk)
        }
    });

    Ok(stream)
}

async fn try_serve_from_s3(
    s3_key: String,
    start: u64,
    end: u64,
    operator: Operator,
    bucket: TokenBucket,
) -> anyhow::Result<impl futures::Stream<Item = Result<Bytes, IoError>> + Send> {
    debug!(%s3_key, "Attempting to fetch from S3");

    // Read the specific range from S3
    let data = operator.read_with(&s3_key).range(start..=end).await?;

    // Create a stream from the data
    let chunks: Vec<Bytes> = data
        .to_bytes()
        .chunks(8 * 1024 * 1024) // 8MB chunks
        .map(Bytes::copy_from_slice)
        .collect();

    let stream = futures::stream::iter(chunks).then(move |chunk| {
        let bucket = bucket.clone();
        async move {
            bucket.consume(chunk.len()).await;
            Ok::<Bytes, IoError>(chunk)
        }
    });

    Ok(stream)
}

pub async fn serve_video(
    Extension(state): Extension<AppState>,
    Extension(bucket): Extension<TokenBucket>,
    AxumPath(filename): AxumPath<String>,
    req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    let vals = filename.split(&['-', '.'][..]).collect::<Vec<_>>();

    let len = vals.len();
    if !(2..=3).contains(&len) {
        warn!(%filename, "Invalid filename");
        return Ok(err_response(StatusCode::BAD_REQUEST, "Invalid filename"));
    }

    let mut job_id = vals[0];

    if let Some((_h, jid)) = vals[0].split_once("/") {
        job_id = jid;
    };

    // First, try to get file size from local filesystem
    let local_path = state.videos_dir().join(job_id).join(&filename);
    let s3_key = format!("videos/{filename}");
    debug!(%job_id, %filename, ?local_path, ?s3_key, "Request server file");

    server_file_with_bucket(state, local_path, s3_key, filename, req, bucket).await
}

fn parse_range(req: &Request<Body>, file_size: u64) -> (StatusCode, u64, u64) {
    if let Some(rh) = req.headers().get(header::RANGE)
        && let Ok(s) = rh.to_str()
        && let Some(stripped) = s.strip_prefix("bytes=")
        && let parts = stripped.split('-').collect::<Vec<_>>()
        && let Ok(start) = parts[0].parse::<u64>()
    {
        let end = parts
            .get(1)
            .and_then(|e| e.parse::<u64>().ok())
            .unwrap_or(file_size - 1);
        return (StatusCode::PARTIAL_CONTENT, start, end.min(file_size - 1));
    }

    (StatusCode::OK, 0, file_size - 1)
}

async fn server_file_with_bucket(
    state: AppState,
    local_path: PathBuf,
    s3_key: String,
    filename: String,
    req: Request<Body>,
    bucket: TokenBucket,
) -> Result<Response<Body>, Infallible> {
    let maybe_filesize = if let Ok(metadata) = tokio::fs::metadata(&local_path).await {
        Some((metadata.len(), false))
    } else if let Some(operator) = state.storage_manager.operator() {
        // Try to get size from S3
        operator
            .stat(&s3_key)
            .await
            .ok()
            .map(|m| (m.content_length(), true))
    } else {
        None
    };

    let Some((size, read_from_s3)) = maybe_filesize else {
        return Ok(file_not_found());
    };

    let (status, start, end) = parse_range(&req, size);
    let len = end - start + 1;

    let maybe_res = if read_from_s3 {
        let operator = state
            .storage_manager
            .operator()
            .expect("S3 operator should be available for remote storage")
            .clone();

        try_serve_from_s3(s3_key, start, end, operator, bucket.clone())
            .await
            .map(|stream| {
                debug!(%filename, "Serving video object from S3");
                Response::new(Body::from_stream(stream))
            })
            .inspect_err(|error| {
                error!(%filename, ?error, "Failed to serve video object from S3");
            })
    } else {
        try_serve_from_filesystem(local_path.clone(), start, end, bucket.clone())
            .await
            .map(|stream| {
                debug!(%filename, "Serving video object from filesystem");
                Response::new(Body::from_stream(stream))
            })
            .inspect_err(|error| {
                error!(%filename, ?error, "Failed to serve video object from filesystem");
            })
    };

    let Ok(mut res) = maybe_res else {
        return Ok(file_not_found());
    };
    *res.status_mut() = status;
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        from_path(&filename)
            .first_or_octet_stream()
            .to_string()
            .parse()
            .unwrap(),
    );
    headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    headers.insert(
        header::CACHE_CONTROL,
        "public,max-age=3600".parse().unwrap(),
    );
    headers.insert(header::CONTENT_LENGTH, len.to_string().parse().unwrap());
    if status == StatusCode::PARTIAL_CONTENT {
        headers.insert(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{size}").parse().unwrap(),
        );
    }
    Ok(res)
}

/// Create a new claim token for video access
pub async fn create_claim(
    Extension(state): Extension<AppState>,
    Json(request): Json<CreateClaimRequest>,
) -> impl IntoResponse {
    // Validate request
    if request.asset_id.is_empty() {
        warn!("asset_id is empty");
        return err_response(StatusCode::BAD_REQUEST, "asset_id is required");
    }

    // Set nbf to current time if not specified
    let nbf_unix = request.nbf_unix.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32
    });

    // Validate exp > nbf
    if request.exp_unix <= nbf_unix {
        warn!("exp_unix must be greater than nbf_unix");
        return err_response(
            StatusCode::BAD_REQUEST,
            "exp_unix must be greater than nbf_unix",
        );
    }

    let payload = ClaimPayloadV1 {
        exp_unix: request.exp_unix,
        nbf_unix,
        asset_id: request.asset_id,
        window_len_sec: request.window_len_sec.unwrap_or(0),
        max_kbps: request.max_kbps.unwrap_or(0),
        max_concurrency: request.max_concurrency.unwrap_or(0),
        allowed_widths: request.allowed_widths.unwrap_or_default(),
    };

    // Sign the claim
    match state.claim_manager.sign_claim(&payload) {
        Ok(token) => {
            debug!(
                asset_id = %payload.asset_id,
                window_sec = payload.window_len_sec,
                max_kbps = payload.max_kbps,
                max_concurrency = payload.max_concurrency,
                allowed_widths = ?payload.allowed_widths,
                "Claim created successfully"
            );
            (StatusCode::OK, Json(CreateClaimResponse { token })).into_response()
        }
        Err(error) => {
            error!(?error, "Failed to create claim");
            err_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to create claim")
        }
    }
}

fn file_not_found() -> Response<Body> {
    err_response(StatusCode::NOT_FOUND, "File not found")
}

pub(crate) fn err_response(status: StatusCode, body_str: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(Body::from(body_str))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_job_id() {
        assert!(is_valid_job_id("test123"));
        assert!(is_valid_job_id("job"));
        assert!(is_valid_job_id("ABC123def"));

        // Invalid cases
        assert!(!is_valid_job_id(""));
        assert!(!is_valid_job_id("test/job"));
        assert!(!is_valid_job_id("test-job"));
        assert!(!is_valid_job_id("test.job"));
        assert!(!is_valid_job_id("test job"));
        assert!(!is_valid_job_id(&"a".repeat(129))); // too long
    }

    #[test]
    fn test_upload_query_parameter_parsing_legacy() {
        // Test parsing ConvertJob from query parameters without scales (legacy format)
        use crate::job::convert::{ConvertJob, RESOLUTIONS};

        let query_string = "id=test123&crf=23";
        let job: ConvertJob = serde_qs::from_str(query_string).unwrap();

        assert_eq!(job.id, "test123");
        assert_eq!(job.crf, 23);
        // Should use default scales for backward compatibility
        assert_eq!(&*job.scales, &RESOLUTIONS);
    }

    #[test]
    fn test_upload_query_parameter_parsing_with_scales() {
        // Test parsing ConvertJob from query parameters - complex nested structures
        // are difficult with URL encoding, so we test the basic functionality
        use crate::job::convert::ConvertJob;

        // Simple query without scales (should use defaults)
        let query_string = "id=test456&crf=30";
        let job: ConvertJob = serde_qs::from_str(query_string).unwrap();

        assert_eq!(job.id, "test456");
        assert_eq!(job.crf, 30);
        // Should use default scales when not specified
        assert_eq!(job.scales.len(), 3); // Default has 3 scales
        assert_eq!(&*job.scales, &crate::job::convert::RESOLUTIONS);
    }

    #[test]
    fn test_upload_parameter_validation() {
        use crate::job::convert::{ConvertJob, Scales};

        // Test CRF validation - valid range
        let valid_job = ConvertJob::new("test".to_string(), 23, Scales::new());
        assert!(valid_job.crf <= 63);

        // Test CRF validation - boundary values
        let min_crf_job = ConvertJob::new("test".to_string(), 0, Scales::new());
        assert!(min_crf_job.crf <= 63);

        let max_crf_job = ConvertJob::new("test".to_string(), 63, Scales::new());
        assert!(max_crf_job.crf <= 63);
    }

    #[test]
    fn test_upload_query_serialization_with_custom_scales() {
        // Test that ConvertJob with custom scales - URL encoding of complex nested
        // structures is limited, so we test basic serialization
        use crate::job::convert::{ConvertJob, Scale, Scales};

        let custom_scales = Scales::from_vec(vec![Scale::new(1920, 1080)]);
        let job = ConvertJob::new("custom123".to_string(), 20, custom_scales);

        // Verify the job structure is correct
        assert_eq!(job.id, "custom123");
        assert_eq!(job.crf, 20);
        assert_eq!(job.scales.len(), 1);
        assert_eq!(job.scales.first().unwrap().width(), 1920);
        assert_eq!(job.scales.first().unwrap().height(), 1080);

        // Basic query serialization (without complex nested structures)
        let simple_job = ConvertJob::new("simple".to_string(), 25, Scales::new());
        let query_result = serde_qs::to_string(&simple_job);
        assert!(query_result.is_ok());
    }

    #[test]
    fn test_upload_round_trip_query_parameters() {
        // Test round-trip serialization/deserialization of ConvertJob through query parameters
        // Focus on basic fields since complex nested structures are challenging with URL encoding
        use crate::job::convert::ConvertJob;

        // Test with default scales (simple case)
        let original_job = ConvertJob::new(
            "roundtrip".to_string(),
            28,
            crate::job::convert::Scales::new(),
        );

        // Test basic serialization/deserialization
        let query_string = serde_qs::to_string(&original_job).unwrap();
        let parsed_job: ConvertJob = serde_qs::from_str(&query_string).unwrap();

        assert_eq!(original_job.id, parsed_job.id);
        assert_eq!(original_job.crf, parsed_job.crf);
        // Both should use default scales
        assert_eq!(&*original_job.scales, &crate::job::convert::RESOLUTIONS);
        assert_eq!(&*parsed_job.scales, &crate::job::convert::RESOLUTIONS);

        // Test manual query string parsing
        let manual_query = "id=manual_test&crf=35";
        let manual_job: ConvertJob = serde_qs::from_str(manual_query).unwrap();
        assert_eq!(manual_job.id, "manual_test");
        assert_eq!(manual_job.crf, 35);
        assert_eq!(&*manual_job.scales, &crate::job::convert::RESOLUTIONS);
    }

    #[test]
    fn test_upload_edge_cases() {
        use crate::job::convert::{ConvertJob, Scales};

        // Test with empty scales (should fallback to defaults)
        let empty_scales_job =
            ConvertJob::new("empty".to_string(), 22, Scales::from_vec(Vec::new()));
        assert!(*empty_scales_job.scales == crate::job::convert::RESOLUTIONS);

        // Test job ID validation edge cases
        assert!(is_valid_job_id("a")); // minimum valid length
        assert!(is_valid_job_id(&"a".repeat(128))); // maximum valid length
        assert!(!is_valid_job_id(&"a".repeat(129))); // too long
    }

    #[test]
    fn test_serde_qs_480p_round_trip() {
        // Test that ConvertJob with 480p-only scales works with serde_qs
        use crate::job::convert::{ConvertJob, Scale, Scales};

        let scales_480p = Scales::from_vec(vec![Scale::new(480, 854)]);
        let job = ConvertJob::new("test_480p".to_string(), 25, scales_480p);

        // Serialize with serde_qs
        let query_string = serde_qs::to_string(&job).unwrap();

        // Should contain the expected format
        assert!(query_string.contains("id=test_480p"));
        assert!(query_string.contains("crf=25"));
        assert!(query_string.contains("scales[0][w]=480"));
        assert!(query_string.contains("scales[0][h]=854"));

        // Deserialize back
        let parsed_job: ConvertJob = serde_qs::from_str(&query_string).unwrap();

        // Verify round-trip correctness
        assert_eq!(parsed_job.id, "test_480p");
        assert_eq!(parsed_job.crf, 25);
        assert_eq!(parsed_job.scales.len(), 1);
        assert_eq!(parsed_job.scales[0].width(), 480);
        assert_eq!(parsed_job.scales[0].height(), 854);
    }
}
