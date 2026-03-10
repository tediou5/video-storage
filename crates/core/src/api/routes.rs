use crate::job::{CONVERT_KIND, UPLOAD_KIND};
use crate::{AppState, ConvertJob, Job};
use axum::body::Body;
use axum::extract::{Extension, Path as AxumPath};
use axum::http::{HeaderValue, Request, Response, StatusCode, header};
use axum::response::{IntoResponse, Json};
use bytes::Bytes;
use futures::StreamExt;
use opendal::EntryMode;
use opendal::Operator;
use opendal::options;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::convert::Infallible;
use std::ffi::OsStr;
use std::io::Error as IoError;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncSeekExt;
use tokio_util::compat::{FuturesAsyncReadCompatExt as _, FuturesAsyncWriteCompatExt as _};
use tokio_util::io::ReaderStream;
use tracing::{debug, error, info, warn};
use video_storage_claim::create_request::AssetsFilter;
use video_storage_claim::payload::ClaimPayloadV2;
use video_storage_claim::{ClaimBucket, ClaimPayloadV1, CreateClaimRequest, CreateClaimResponse};
use xorf::BinaryFuse16;

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

#[derive(Serialize, Deserialize)]
pub struct MigrateResponse {
    pub job_id: String,
    pub src_bucket: String,
    pub dst_bucket: String,
    pub dry_run: bool,
    pub objects_total: u64,
    pub objects_copied: u64,
    pub bytes_copied: u64,
}

#[derive(Serialize, Deserialize)]
pub struct MigrateRequest {
    pub job_id: String,
    pub src_bucket: String,
    pub dst_bucket: String,
    #[serde(default)]
    pub widths: Option<Vec<u16>>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Serialize, Deserialize)]
pub struct MigrateErrorResponse {
    pub job_id: String,
    pub message: String,
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

    if state.storage_manager.is_s3() {
        let Some(dst_bucket) = job.dst_bucket.as_deref().filter(|b| !b.is_empty()) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(UploadResponse {
                    job_id,
                    message: "Missing dst_bucket (required when storage_backend is s3)".into(),
                }),
            );
        };
        if let Err(message) = validate_bucket_for_write(dst_bucket) {
            return (
                StatusCode::BAD_REQUEST,
                Json(UploadResponse {
                    job_id,
                    message: message.into(),
                }),
            );
        }
    }

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
    bucket: ClaimBucket,
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
    bucket: ClaimBucket,
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

fn is_pure_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

fn validate_bucket_for_write(bucket: &str) -> Result<(), &'static str> {
    if bucket.is_empty() {
        return Err("Invalid bucket: empty");
    }
    if bucket.contains('/') {
        return Err("Invalid bucket: contains '/'");
    }
    if is_pure_ascii_digits(bucket) {
        return Err("Invalid bucket: numeric-only bucket is not allowed");
    }
    Ok(())
}

fn split_bucket_and_key(filename: &str, is_s3: bool) -> Result<(Option<&str>, &str), &'static str> {
    if is_s3 {
        let Some((bucket, key)) = filename.split_once('/') else {
            return Err("Missing bucket in /videos path");
        };
        if bucket.is_empty() || key.is_empty() {
            return Err("Invalid /videos path");
        }
        return Ok((Some(bucket), key));
    }

    let Some((first, rest)) = filename.split_once('/') else {
        return Ok((None, filename));
    };

    // Local backend:
    // - Keep legacy layout: `720/<file>` (first segment is numeric width)
    // - Allow bucket-prefixed paths: `<bucket>/<key>` by stripping the first segment
    if first.chars().all(|c| c.is_ascii_digit()) {
        Ok((None, filename))
    } else if rest.is_empty() {
        Err("Invalid /videos path")
    } else {
        Ok((Some(first), rest))
    }
}

fn extract_job_id_from_key(key: &str) -> Result<&str, &'static str> {
    let path_and_suffix = key.split('.').collect::<Vec<_>>();
    if path_and_suffix.len() != 2 {
        return Err("Invalid filename");
    }

    let path = path_and_suffix[0];
    let vals = path.split('-').collect::<Vec<_>>();
    if !(1..=3).contains(&vals.len()) {
        return Err("Invalid filename");
    }

    let mut job_id = vals[0];
    if let Some((_h, jid)) = vals[0].split_once('/') {
        job_id = jid;
    }

    Ok(job_id)
}

pub async fn serve_video(
    Extension(state): Extension<AppState>,
    Extension(bucket): Extension<ClaimBucket>,
    AxumPath(filename): AxumPath<String>,
    req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    let is_s3 = state.storage_manager.is_s3();
    let (bucket_prefix, key) = match split_bucket_and_key(&filename, is_s3) {
        Ok(v) => v,
        Err(msg) => return Ok(err_response(StatusCode::BAD_REQUEST, msg)),
    };

    let operator = if is_s3 {
        let Some(bucket_name) = bucket_prefix else {
            return Ok(err_response(StatusCode::BAD_REQUEST, "Missing bucket"));
        };
        match state.storage_manager.operator_for_bucket(bucket_name) {
            Ok(op) => Some(op),
            Err(error) => {
                warn!(?error, bucket = %bucket_name, "Invalid bucket");
                return Ok(err_response(StatusCode::BAD_REQUEST, "Invalid bucket"));
            }
        }
    } else {
        None
    };

    let job_id = match extract_job_id_from_key(key) {
        Ok(v) => v,
        Err(msg) => {
            warn!(filename = %key, "Invalid filename");
            return Ok(err_response(StatusCode::BAD_REQUEST, msg));
        }
    };

    // First, try to get file size from local filesystem
    let local_path = state.videos_dir().join(job_id).join(key);
    let s3_key = format!("videos/{key}");
    debug!(
        %job_id,
        filename = %key,
        bucket = bucket_prefix.unwrap_or(""),
        ?local_path,
        ?s3_key,
        "Request server file"
    );

    server_file_with_bucket(local_path, s3_key, key.to_string(), req, bucket, operator).await
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
    local_path: PathBuf,
    s3_key: String,
    filename: String,
    req: Request<Body>,
    bucket: ClaimBucket,
    operator: Option<Operator>,
) -> Result<Response<Body>, Infallible> {
    let maybe_filesize = if let Ok(metadata) = tokio::fs::metadata(&local_path).await {
        Some((metadata.len(), false))
    } else if let Some(operator) = operator.as_ref() {
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
        let operator = operator.expect("S3 operator should be available for remote storage");

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
    headers.insert(header::CONTENT_TYPE, hls_content_type(Path::new(&filename)));
    headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    headers.insert(header::CACHE_CONTROL, "no-transform".parse().unwrap());
    headers.insert(header::CONTENT_LENGTH, len.to_string().parse().unwrap());
    if status == StatusCode::PARTIAL_CONTENT {
        headers.insert(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{size}").parse().unwrap(),
        );
    }
    Ok(res)
}

pub async fn migrate_videos(
    Extension(state): Extension<AppState>,
    Json(request): Json<MigrateRequest>,
) -> impl IntoResponse {
    let job_id = request.job_id.clone();

    let migrate_err = |status: StatusCode, message: String| {
        (
            status,
            Json(MigrateErrorResponse {
                job_id: job_id.clone(),
                message,
            }),
        )
            .into_response()
    };

    if !state.storage_manager.is_s3() {
        return migrate_err(StatusCode::BAD_REQUEST, "storage_backend is not s3".into());
    }

    if !is_valid_job_id(&job_id) {
        return migrate_err(StatusCode::BAD_REQUEST, "Invalid job ID format".into());
    }

    if let Err(message) = validate_bucket_for_write(&request.src_bucket) {
        return migrate_err(
            StatusCode::BAD_REQUEST,
            format!("Invalid src_bucket: {message}"),
        );
    }
    if let Err(message) = validate_bucket_for_write(&request.dst_bucket) {
        return migrate_err(
            StatusCode::BAD_REQUEST,
            format!("Invalid dst_bucket: {message}"),
        );
    }

    let src_op = match state
        .storage_manager
        .operator_for_bucket(&request.src_bucket)
    {
        Ok(op) => op,
        Err(error) => {
            warn!(?error, bucket = %request.src_bucket, "Invalid src_bucket");
            return migrate_err(StatusCode::BAD_REQUEST, "Invalid src_bucket".into());
        }
    };
    let dst_op = match state
        .storage_manager
        .operator_for_bucket(&request.dst_bucket)
    {
        Ok(op) => op,
        Err(error) => {
            warn!(?error, bucket = %request.dst_bucket, "Invalid dst_bucket");
            return migrate_err(StatusCode::BAD_REQUEST, "Invalid dst_bucket".into());
        }
    };

    let widths = request.widths.unwrap_or_else(|| {
        crate::job::convert::RESOLUTIONS
            .iter()
            .map(|s| u16::try_from(s.width()).expect("resolution width must fit in u16"))
            .collect()
    });
    let mut prefixes = Vec::with_capacity(widths.len() + 1);
    prefixes.push(format!("videos/{job_id}"));
    for w in widths {
        prefixes.push(format!("videos/{w}/{job_id}"));
    }

    let mut keys: BTreeSet<String> = BTreeSet::new();
    for prefix in prefixes {
        let mut lister = match src_op
            .lister_options(
                &prefix,
                options::ListOptions {
                    recursive: true,
                    ..Default::default()
                },
            )
            .await
        {
            Ok(lister) => lister,
            Err(error) => {
                error!(?error, %prefix, "Failed to list src objects");
                return migrate_err(StatusCode::INTERNAL_SERVER_ERROR, "List failed".into());
            }
        };

        use futures::TryStreamExt as _;
        while let Some(entry) = match lister.try_next().await {
            Ok(v) => v,
            Err(error) => {
                error!(?error, %prefix, "Failed to iterate src objects");
                return migrate_err(StatusCode::INTERNAL_SERVER_ERROR, "List failed".into());
            }
        } {
            if entry.metadata().mode() != EntryMode::FILE {
                continue;
            }
            let path = entry.path();
            let Some(rest) = path.strip_prefix(&prefix) else {
                continue;
            };
            if !(rest.starts_with('.') || rest.starts_with('-')) {
                continue;
            }
            _ = keys.insert(path.to_string());
        }
    }

    let mut objects_total = 0u64;
    let mut objects_copied = 0u64;
    let mut bytes_copied = 0u64;

    const DELETE_SOURCE_OBJECTS: bool = false;

    for key in keys {
        objects_total += 1;

        let meta = match src_op.stat(&key).await {
            Ok(m) => m,
            Err(error) => {
                error!(?error, %key, "Failed to stat src object");
                return migrate_err(StatusCode::INTERNAL_SERVER_ERROR, "Stat failed".into());
            }
        };

        let size = meta.content_length();
        if request.dry_run {
            bytes_copied += size;
            objects_copied += 1;
            continue;
        }

        let copied = match copy_object_streaming(&src_op, &dst_op, &key, meta.content_type()).await
        {
            Ok(v) => v,
            Err(error) => {
                error!(?error, %key, "Failed to copy object");
                return migrate_err(StatusCode::INTERNAL_SERVER_ERROR, "Copy failed".into());
            }
        };

        bytes_copied += copied;
        objects_copied += 1;

        // NOTE: Deletion is intentionally disabled in this release to prevent accidental data loss.
        if DELETE_SOURCE_OBJECTS && let Err(error) = src_op.delete(&key).await {
            error!(?error, %key, "Failed to delete src object");
            return migrate_err(StatusCode::INTERNAL_SERVER_ERROR, "Delete failed".into());
        }
    }

    (
        StatusCode::OK,
        Json(MigrateResponse {
            job_id,
            src_bucket: request.src_bucket,
            dst_bucket: request.dst_bucket,
            dry_run: request.dry_run,
            objects_total,
            objects_copied,
            bytes_copied,
        }),
    )
        .into_response()
}

async fn copy_object_streaming(
    src: &Operator,
    dst: &Operator,
    key: &str,
    content_type: Option<&str>,
) -> anyhow::Result<u64> {
    let reader = src.reader(key).await?;
    let mut r = reader.into_futures_async_read(..).await?.compat();

    let mut writer = dst.writer_with(key).chunk(8 * 1024 * 1024).concurrent(8);
    if let Some(ct) = content_type {
        writer = writer.content_type(ct);
    }
    let mut w = writer.await?.into_futures_async_write().compat_write();

    let copied = tokio::io::copy(&mut r, &mut w).await?;
    use tokio::io::AsyncWriteExt as _;
    w.shutdown().await?;

    Ok(copied)
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

    let res = match request.asset_id {
        AssetsFilter::One(asset_id) => {
            let payload = ClaimPayloadV1 {
                exp_unix: request.exp_unix,
                nbf_unix,
                asset_id,
                window_len_sec: request.window_len_sec.unwrap_or(0),
                max_kbps: request.max_kbps.unwrap_or(0),
                max_concurrency: request.max_concurrency.unwrap_or(0),
                allowed_widths: request.allowed_widths.unwrap_or_default(),
            };
            state.claim_manager.sign_claim(&payload)
        }
        AssetsFilter::List(filter) => {
            let Ok(assets_filter) = BinaryFuse16::try_from(filter.as_slice()) else {
                return err_response(StatusCode::BAD_REQUEST, "Invalid assets filter");
            };
            let payload = ClaimPayloadV2 {
                exp_unix: request.exp_unix,
                nbf_unix,
                assets_filter,
                window_len_sec: request.window_len_sec.unwrap_or(0),
                max_kbps: request.max_kbps.unwrap_or(0),
                max_concurrency: request.max_concurrency.unwrap_or(0),
                allowed_widths: request.allowed_widths.unwrap_or_default(),
            };
            state.claim_manager.sign_claim(&payload)
        }
    };

    // Sign the claim
    match res {
        Ok(token) => {
            debug!("Claim created successfully");
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

fn hls_content_type(path: &Path) -> HeaderValue {
    HeaderValue::from_static(
        match path
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .unwrap_or_default()
            .as_str()
        {
            "m3u8" => "application/vnd.apple.mpegurl",
            "m4s" => "video/iso.segment",
            "mp4" => "video/mp4",
            _ => "application/octet-stream",
        },
    )
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
        assert_eq!(&*job.codecs, &crate::job::convert::DEFAULT_CODECS);
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
        let query_string = query_result.unwrap();
        // Default codecs should be skipped for backward compatibility
        assert!(!query_string.contains("codecs"));
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
        assert_eq!(&*parsed_job.codecs, &crate::job::convert::DEFAULT_CODECS);

        // Test manual query string parsing
        let manual_query = "id=manual_test&crf=35";
        let manual_job: ConvertJob = serde_qs::from_str(manual_query).unwrap();
        assert_eq!(manual_job.id, "manual_test");
        assert_eq!(manual_job.crf, 35);
        assert_eq!(&*manual_job.scales, &crate::job::convert::RESOLUTIONS);
        assert_eq!(&*manual_job.codecs, &crate::job::convert::DEFAULT_CODECS);
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
        assert_eq!(&*parsed_job.codecs, &crate::job::convert::DEFAULT_CODECS);
    }

    #[test]
    fn test_upload_query_parameter_parsing_with_codecs() {
        use crate::job::convert::{ConvertCodec, ConvertJob};

        let query_string = "id=codec_job&crf=32&codecs[0]=h265";
        let job: ConvertJob = serde_qs::from_str(query_string).unwrap();

        assert_eq!(job.id, "codec_job");
        assert_eq!(job.crf, 32);
        assert_eq!(job.codecs.len(), 1);
        assert_eq!(job.codecs[0], ConvertCodec::H265);
    }

    #[tokio::test]
    async fn test_upload_mp4_raw_accepts_h265_only() {
        use crate::opendal::{StorageBackend, StorageConfig, StorageManager};
        use axum::body::Body;
        use axum::http::Request;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let storage_manager = StorageManager::new(StorageConfig {
            backend: StorageBackend::Local,
            workspace: temp_dir.path().to_path_buf(),
        })
        .await
        .unwrap();

        // Use zero permits so background worker never starts processing the job during the test
        let state = AppState::new(0, temp_dir.path(), storage_manager, None, Vec::new())
            .await
            .unwrap();

        let job_id = "codecjob";
        let uri = format!("/upload?id={job_id}&crf=28&codecs[0]=h265");
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "video/mp4")
            .body(Body::from("fake mp4 data"))
            .unwrap();

        let response = upload_mp4_raw(Extension(state.clone()), request)
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let upload_response: UploadResponse = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(upload_response.job_id, job_id);

        let jobs = state.jobs_manager.jobs.lock().await;
        let job_json = jobs
            .iter()
            .find_map(|job| {
                if job.id() == job_id {
                    Some(job.to_json())
                } else {
                    None
                }
            })
            .expect("job should be enqueued");
        assert_eq!(job_json["codecs"], serde_json::json!(["h265"]));
    }

    #[test]
    fn test_split_bucket_and_key_local_legacy_and_bucket_prefixed() {
        assert_eq!(
            split_bucket_and_key("test_video.m3u8", false).unwrap(),
            (None, "test_video.m3u8")
        );
        assert_eq!(
            split_bucket_and_key("720/test_video.m3u8", false).unwrap(),
            (None, "720/test_video.m3u8")
        );
        assert_eq!(
            split_bucket_and_key("mybucket/test_video.m3u8", false).unwrap(),
            (Some("mybucket"), "test_video.m3u8")
        );
        assert_eq!(
            split_bucket_and_key("mybucket/720/test_video.m3u8", false).unwrap(),
            (Some("mybucket"), "720/test_video.m3u8")
        );
    }

    #[test]
    fn test_split_bucket_and_key_s3_requires_bucket() {
        assert!(split_bucket_and_key("test_video.m3u8", true).is_err());
        assert_eq!(
            split_bucket_and_key("mybucket/test_video.m3u8", true).unwrap(),
            (Some("mybucket"), "test_video.m3u8")
        );
    }

    #[test]
    fn test_extract_job_id_from_key() {
        assert_eq!(
            extract_job_id_from_key("test_video.m3u8").unwrap(),
            "test_video"
        );
        assert_eq!(
            extract_job_id_from_key("test_video-h265.m3u8").unwrap(),
            "test_video"
        );
        assert_eq!(
            extract_job_id_from_key("720/test_video.m3u8").unwrap(),
            "test_video"
        );
        assert_eq!(
            extract_job_id_from_key("720/test_video-h265-001.m4s").unwrap(),
            "test_video"
        );
        assert!(extract_job_id_from_key("bad.name.m3u8").is_err());
        assert!(extract_job_id_from_key("a-b-c-d.m3u8").is_err());
    }

    #[test]
    fn test_validate_bucket_for_write_rejects_numeric_only() {
        assert!(validate_bucket_for_write("mybucket").is_ok());
        assert!(validate_bucket_for_write("123").is_err());
        assert!(validate_bucket_for_write("bucket/123").is_err());
    }

    #[tokio::test]
    async fn test_upload_mp4_raw_requires_dst_bucket_when_s3() {
        use crate::opendal::{StorageBackend, StorageConfig, StorageManager};
        use axum::body::Body;
        use axum::http::Request;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let storage_manager = StorageManager::new(StorageConfig {
            backend: StorageBackend::S3 {
                endpoint: Some("http://127.0.0.1:9000".into()),
                region: Some("us-east-1".into()),
                access_key_id: "minioadmin".into(),
                secret_access_key: "minioadmin".into(),
            },
            workspace: temp_dir.path().to_path_buf(),
        })
        .await
        .unwrap();

        let state = AppState::new(0, temp_dir.path(), storage_manager, None, Vec::new())
            .await
            .unwrap();

        let request = Request::builder()
            .method("POST")
            .uri("/upload?id=test123&crf=23")
            .header(header::CONTENT_TYPE, "video/mp4")
            .body(Body::from("fake mp4 data"))
            .unwrap();

        let response = upload_mp4_raw(Extension(state), request)
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let upload_response: UploadResponse = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(upload_response.job_id, "test123");
        assert!(upload_response.message.contains("dst_bucket"));
    }

    #[tokio::test]
    async fn test_upload_mp4_raw_rejects_numeric_only_dst_bucket_when_s3() {
        use crate::opendal::{StorageBackend, StorageConfig, StorageManager};
        use axum::body::Body;
        use axum::http::Request;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let storage_manager = StorageManager::new(StorageConfig {
            backend: StorageBackend::S3 {
                endpoint: Some("http://127.0.0.1:9000".into()),
                region: Some("us-east-1".into()),
                access_key_id: "minioadmin".into(),
                secret_access_key: "minioadmin".into(),
            },
            workspace: temp_dir.path().to_path_buf(),
        })
        .await
        .unwrap();

        let state = AppState::new(0, temp_dir.path(), storage_manager, None, Vec::new())
            .await
            .unwrap();

        let request = Request::builder()
            .method("POST")
            .uri("/upload?id=test123&crf=23&dst_bucket=123")
            .header(header::CONTENT_TYPE, "video/mp4")
            .body(Body::from("fake mp4 data"))
            .unwrap();

        let response = upload_mp4_raw(Extension(state), request)
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let upload_response: UploadResponse = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(upload_response.job_id, "test123");
        assert!(upload_response.message.contains("numeric-only"));
    }

    #[tokio::test]
    async fn test_migrate_videos_requires_s3_backend() {
        use crate::opendal::{StorageBackend, StorageConfig, StorageManager};
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let storage_manager = StorageManager::new(StorageConfig {
            backend: StorageBackend::Local,
            workspace: temp_dir.path().to_path_buf(),
        })
        .await
        .unwrap();

        let state = AppState::new(0, temp_dir.path(), storage_manager, None, Vec::new())
            .await
            .unwrap();

        let request = MigrateRequest {
            src_bucket: "src".into(),
            dst_bucket: "dst".into(),
            job_id: "test123".into(),
            dry_run: true,
            widths: None,
        };

        let response = migrate_videos(Extension(state), Json(request))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let err: MigrateErrorResponse = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(err.job_id, "test123");
        assert!(err.message.contains("not s3"));
    }

    #[tokio::test]
    async fn test_migrate_videos_rejects_numeric_only_buckets() {
        use crate::opendal::{StorageBackend, StorageConfig, StorageManager};
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let storage_manager = StorageManager::new(StorageConfig {
            backend: StorageBackend::S3 {
                endpoint: Some("http://127.0.0.1:9000".into()),
                region: Some("us-east-1".into()),
                access_key_id: "minioadmin".into(),
                secret_access_key: "minioadmin".into(),
            },
            workspace: temp_dir.path().to_path_buf(),
        })
        .await
        .unwrap();

        let state = AppState::new(0, temp_dir.path(), storage_manager, None, Vec::new())
            .await
            .unwrap();

        let request = MigrateRequest {
            src_bucket: "123".into(),
            dst_bucket: "dst".into(),
            job_id: "test123".into(),
            dry_run: true,
            widths: None,
        };

        let response = migrate_videos(Extension(state), Json(request))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let err: MigrateErrorResponse = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(err.job_id, "test123");
        assert!(err.message.contains("numeric-only"));
    }
}
