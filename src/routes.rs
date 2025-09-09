use crate::app_state::VIDEO_OBJECTS_DIR;
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
use std::io::Error as IoError;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::create_dir_all;
use tokio::io::AsyncSeekExt;
use tokio::sync::Mutex as TokioMutex;
use tokio_util::io::ReaderStream;
use tracing::{debug, error, info, warn};

#[derive(Serialize, Deserialize)]
pub(crate) struct UploadResponse {
    pub(crate) job_id: String,
    pub(crate) message: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct WaitlistResponse {
    pub(crate) pending_convert_jobs: usize,
    pub(crate) pending_upload_jobs: usize,
    pub(crate) total_pending_jobs: usize,
}

#[axum::debug_handler]
pub(crate) async fn waitlist(Extension(state): Extension<AppState>) -> impl IntoResponse {
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

pub(crate) async fn upload_mp4_raw(
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
    token_rate: f64,
) -> Result<impl futures::Stream<Item = Result<Bytes, IoError>> + Send, String> {
    let mut fh = tokio::fs::File::open(&path)
        .await
        .map_err(|e| e.to_string())?;

    fh.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|e| e.to_string())?;
    let len = end - start + 1;

    let bucket = Arc::new(TokioMutex::new(TokenBucket::new(token_rate, token_rate)));

    use tokio::io::AsyncReadExt as _;
    let stream = ReaderStream::new(fh.take(len))
        .map_err(|e| IoError::new(e.kind(), e.to_string()))
        .then(move |res| {
            let bucket = bucket.clone();
            async move {
                match res {
                    Ok(chunk) => {
                        bucket.lock().await.consume(chunk.len()).await;
                        Ok::<Bytes, IoError>(chunk)
                    }
                    Err(e) => Err(e),
                }
            }
        });

    Ok(stream)
}

async fn try_serve_from_s3(
    filename: String,
    start: u64,
    end: u64,
    operator: Operator,
    token_rate: f64,
) -> Result<impl futures::Stream<Item = Result<Bytes, IoError>> + Send, String> {
    // S3 path is videos/filename (without job_id directory)
    let s3_key = format!("videos/{}", filename);
    debug!(%s3_key, "Attempting to fetch from S3");

    // Read the specific range from S3
    let data = operator
        .read_with(&s3_key)
        .range(start..=end)
        .await
        .map_err(|e| e.to_string())?;

    let bucket = Arc::new(TokioMutex::new(TokenBucket::new(token_rate, token_rate)));

    // Create a stream from the data
    let chunks: Vec<Bytes> = data
        .to_bytes()
        .chunks(8 * 1024 * 1024) // 8MB chunks
        .map(|chunk| Bytes::copy_from_slice(chunk))
        .collect();

    let stream = futures::stream::iter(chunks).then(move |chunk| {
        let bucket = bucket.clone();
        async move {
            bucket.lock().await.consume(chunk.len()).await;
            Ok::<Bytes, IoError>(chunk)
        }
    });

    Ok(stream)
}

pub(crate) async fn serve_video(
    Extension(state): Extension<AppState>,
    AxumPath(filename): AxumPath<String>,
    req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    let vals = filename.split(&['-', '.'][..]).collect::<Vec<_>>();

    let len = vals.len();
    if !(2..=3).contains(&len) {
        warn!(%filename, "Invalid filename");
        return Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("Invalid filename"))
            .unwrap());
    }

    let mut job_id = vals[0];

    if let Some((_h, jid)) = vals[0].split_once("/") {
        job_id = jid;
    };

    // First, try to get file size from local filesystem
    let local_path = state.videos_dir().join(job_id).join(&filename);
    debug!(%job_id, %filename, ?local_path, "Request server file");

    let file_size = if let Ok(metadata) = tokio::fs::metadata(&local_path).await {
        Some(metadata.len())
    } else if state.storage_manager.is_remote() {
        // Try to get size from S3
        let s3_key = format!("videos/{}", filename);
        state
            .storage_manager
            .operator()
            .stat(&s3_key)
            .await
            .inspect_err(|error| debug!(?error, "file state not found in s3"))
            .map(|m| m.content_length())
            .ok()
    } else {
        debug!(%job_id, %filename, "file metadata not found");
        None
    };

    let Some(size) = file_size else {
        debug!(%job_id, %filename, "file size not found");
        return Ok(file_not_found());
    };

    let (status, start, end) = parse_range(&req, size);
    let len = end - start + 1;

    // Try to get stream from filesystem first, then S3
    let maybe_res = if let Ok(stream) =
        try_serve_from_filesystem(local_path.clone(), start, end, state.token_rate).await
    {
        debug!(%filename, "Serving from filesystem");
        Some(Response::new(Body::from_stream(stream)))
    } else if state.storage_manager.is_remote() {
        let operator = state.storage_manager.operator().clone();
        match try_serve_from_s3(filename.clone(), start, end, operator, state.token_rate).await {
            Ok(stream) => {
                debug!(%filename, "Serving from S3");
                Some(Response::new(Body::from_stream(stream)))
            }
            Err(e) => {
                error!(%filename, ?e, "Failed to serve from S3");
                None
            }
        }
    } else {
        None
    };

    let Some(mut res) = maybe_res else {
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

fn file_not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("File not found"))
        .unwrap()
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
pub(crate) struct UploadFilesRequest {
    id: String,
    name: String,
}

#[axum::debug_handler]
pub(crate) async fn upload_files(
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

    (
        StatusCode::OK,
        Json(UploadResponse {
            job_id,
            message: "Upload object done".into(),
        }),
    )
}

pub(crate) async fn serve_video_object(
    Extension(state): Extension<AppState>,
    AxumPath((job_id, filename)): AxumPath<(String, String)>,
    req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    let path = state
        .videos_dir()
        .join(&job_id)
        .join(VIDEO_OBJECTS_DIR)
        .join(&filename);
    debug!(%job_id, %filename, ?path, "Request server file");

    let Ok(mut fh) = tokio::fs::File::open(&path).await else {
        return Ok(file_not_found());
    };
    let Ok(metadata) = fh.metadata().await else {
        return Ok(file_not_found());
    };

    let size = metadata.len();

    let (status, start, end) = parse_range(&req, size);
    fh.seek(std::io::SeekFrom::Start(start)).await.unwrap(); // seek to start, just panic if failed
    let len = end - start + 1;

    let bucket = Arc::new(TokioMutex::new(TokenBucket::new(
        state.token_rate,
        state.token_rate,
    )));

    use tokio::io::AsyncReadExt as _;
    let stream = ReaderStream::new(fh.take(len))
        .map_err(|e| IoError::new(e.kind(), e.to_string()))
        .then(move |res| {
            let bucket = bucket.clone();
            async move {
                match res {
                    Ok(chunk) => {
                        bucket.lock().await.consume(chunk.len()).await;
                        Ok::<Bytes, IoError>(chunk)
                    }
                    Err(e) => Err(e),
                }
            }
        });

    let mut res = Response::new(Body::from_stream(stream));
    *res.status_mut() = status;
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        from_path(&path)
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
