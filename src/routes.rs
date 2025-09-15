use crate::app_state::VIDEO_OBJECTS_DIR;
use crate::claim::{ClaimPayloadV1, CreateClaimRequest, CreateClaimResponse};
use crate::{AppState, ConvertJob, TokenBucket};
use axum::body::Body;
use axum::extract::{Extension, Path as AxumPath, Query};
use axum::http::{Request, Response, StatusCode, header};
use axum::response::{IntoResponse, Json};
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use mime_guess::from_path;
use opendal::Operator;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::future::Future;
use std::io::Error as IoError;
use std::path::PathBuf;

use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs::create_dir_all;
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

#[axum::debug_handler]
pub async fn waitlist(Extension(state): Extension<AppState>) -> impl IntoResponse {
    let convert_jobs = state.job_set.lock().await.len();
    let upload_jobs = state.upload_job_set.lock().await.len();

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
    Query(job): Query<ConvertJob>,
    body: Body,
) -> impl IntoResponse {
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

    if state.job_set.lock().await.contains(&job) {
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                job_id: job.id,
                message: "already in-progress".into(),
            }),
        );
    }

    if job_id.is_empty()
        || job_id.contains('/')
        || job_id.contains('-')
        || job_id.contains('.')
        || job_id.contains(' ')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                job_id,
                message: "Invalid filename, Cannot contain '/', '-', ' ' and '.'".into(),
            }),
        );
    }

    info!(%job_id, "Uploading file");

    let upload_path = state.uploads_dir().join(&job_id);
    let Ok(mut file) = tokio::fs::File::create(&upload_path).await else {
        error!(%job_id, "Failed to create upload job file");
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                job_id,
                message: "Failed to create upload job file".into(),
            }),
        );
    };

    use tokio::io::AsyncWriteExt as _;
    let mut body_stream = body.into_data_stream();
    while let Some(Ok(chunk)) = body_stream.next().await {
        if file.write_all(&chunk).await.is_err() {
            // clean up upload job file
            error!(%job_id, "Failed to write to upload job file");
            let _ = tokio::fs::remove_file(&upload_path).await;

            return (
                StatusCode::BAD_REQUEST,
                Json(UploadResponse {
                    job_id,
                    message: "Failed to write to upload job file".into(),
                }),
            );
        }
    }

    let generator = crate::JobGenerator::new(job, upload_path);
    _ = state.job_tx.unbounded_send(generator.into());

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
) -> Result<impl futures::Stream<Item = Result<Bytes, IoError>> + Send, String> {
    let mut fh = tokio::fs::File::open(&path)
        .await
        .map_err(|e| e.to_string())?;

    fh.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|e| e.to_string())?;
    let len = end - start + 1;

    use tokio::io::AsyncReadExt as _;
    let stream = ReaderStream::new(fh.take(len))
        .map_err(|e| IoError::new(e.kind(), e.to_string()))
        .then(move |res| {
            let bucket = bucket.clone();
            async move {
                match res {
                    Ok(chunk) => {
                        bucket.consume(chunk.len()).await;
                        Ok::<Bytes, IoError>(chunk)
                    }
                    Err(e) => Err(e),
                }
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
) -> Result<impl futures::Stream<Item = Result<Bytes, IoError>> + Send, String> {
    debug!(%s3_key, "Attempting to fetch from S3");

    // Read the specific range from S3
    let data = operator
        .read_with(&s3_key)
        .range(start..=end)
        .await
        .map_err(|e| e.to_string())?;

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

#[derive(Serialize, Deserialize)]
pub struct UploadFilesRequest {
    id: String,
    name: String,
}

#[axum::debug_handler]
pub async fn upload_files(
    Extension(state): Extension<AppState>,
    Query(UploadFilesRequest { id: job_id, name }): Query<UploadFilesRequest>,
    body: Body,
) -> impl IntoResponse {
    if job_id.is_empty()
        || job_id.contains('/')
        || job_id.contains('-')
        || job_id.contains('.')
        || job_id.contains(' ')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                job_id,
                message: "Invalid id, Cannot contain '/', '-', ' ' and '.'".into(),
            }),
        );
    }

    if name.is_empty() || name.contains('/') || name.contains(' ') {
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                job_id,
                message: "Invalid object name, Cannot contain '/' and ' '".into(),
            }),
        );
    }

    info!(%job_id, name, "Uploading video objects file");
    let upload_dir = state.videos_dir().join(&job_id).join(VIDEO_OBJECTS_DIR);
    create_dir_all(&upload_dir).await.unwrap();

    let upload_path = upload_dir.join(&name);
    let Ok(mut file) = tokio::fs::File::create(&upload_path).await else {
        error!(%job_id, %name, "Failed to create upload job object file");
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                job_id,
                message: "Failed to create upload job object file".into(),
            }),
        );
    };

    use tokio::io::AsyncWriteExt as _;
    let mut body_stream = body.into_data_stream();
    while let Some(Ok(chunk)) = body_stream.next().await {
        if file.write_all(&chunk).await.is_err() {
            // clean up upload job object file
            error!(%job_id, %name, "Failed to write to upload job object file");
            let _ = tokio::fs::remove_file(&upload_path).await;

            return (
                StatusCode::BAD_REQUEST,
                Json(UploadResponse {
                    job_id,
                    message: "Failed to write to upload job object file".into(),
                }),
            );
        }
    }

    // Upload to S3 if remote storage is configured
    if state.storage_manager.is_remote() {
        let s3_key = format!("videos/{}/{}/{}", job_id, VIDEO_OBJECTS_DIR, name);
        debug!(%s3_key, "Uploading video object to S3");

        if let Err(e) = state
            .storage_manager
            .operator()
            .write(&s3_key, tokio::fs::read(&upload_path).await.unwrap())
            .await
        {
            error!(%job_id, %name, ?e, "Failed to upload video object to S3");
        } else {
            info!(%job_id, %name, "Video object uploaded to S3 successfully");
        }
    }

    (
        StatusCode::OK,
        Json(UploadResponse {
            job_id,
            message: "Upload object done".into(),
        }),
    )
}

pub fn serve_video_object(
    Extension(state): Extension<AppState>,
    Extension(bucket): Extension<TokenBucket>,
    AxumPath((job_id, filename)): AxumPath<(String, String)>,
    req: Request<Body>,
) -> impl Future<Output = Result<Response<Body>, Infallible>> {
    // First, try to get file size from local filesystem
    let local_path = state
        .videos_dir()
        .join(&job_id)
        .join(VIDEO_OBJECTS_DIR)
        .join(&filename);
    let s3_key = format!("videos/{job_id}/{VIDEO_OBJECTS_DIR}/{filename}");
    debug!(%job_id, %filename, ?local_path, ?s3_key, "Request server video object file");

    server_file_with_bucket(state, local_path, s3_key, filename, req, bucket)
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
    } else if state.storage_manager.is_remote() {
        // Try to get size from S3
        state
            .storage_manager
            .operator()
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
        let operator = state.storage_manager.operator().clone();

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
        window_len_sec: request.window_len_sec,
        max_kbps: request.max_kbps,
        max_concurrency: request.max_concurrency,
        allowed_widths: request.allowed_widths,
    };

    // Sign the claim
    match state.claim_manager.sign_claim(&payload).await {
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
        Err(err) => {
            error!("Failed to create claim: {err}");
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
