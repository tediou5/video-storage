use crate::{AppState, Job, TokenBucket};

use std::{convert::Infallible, io::Error as IoError, path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    extract::{Extension, Path as AxumPath, Query},
    http::{Request, Response, StatusCode, header},
    response::{IntoResponse, Json},
};
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use mime_guess::from_path;
use serde::{Deserialize, Serialize};
use tokio::{fs::create_dir_all, io::AsyncSeekExt, sync::Mutex as TokioMutex};
use tokio_util::io::ReaderStream;
use tracing::{debug, error, info, warn};

/// bytes per second (500 KB/s)
const RATE_BYTES_PER_SEC: f64 = 500.0 * 1024.0;

#[derive(Serialize, Deserialize)]
pub(crate) struct UploadResponse {
    pub(crate) job_id: String,
    pub(crate) message: String,
}

pub(crate) async fn upload_mp4_raw(
    Extension(state): Extension<AppState>,
    Query(job): Query<Job>,
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
    let upload_dir = PathBuf::from(crate::UPLOADS_DIR);
    create_dir_all(&upload_dir).await.unwrap();

    let upload_path = upload_dir.join(&job_id);
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

    let mut jobs = state.job_set.lock().await;
    jobs.insert(job.clone());
    let dump = serde_json::to_string(&*jobs).unwrap();
    tokio::fs::write(crate::JOB_FILE, dump).await.unwrap();
    info!(%job_id, "File uploaded successfully, jobs set updated");

    let generator = crate::JobGenerator::new(job, upload_path);

    _ = state.job_tx.unbounded_send(generator);

    (
        StatusCode::ACCEPTED,
        Json(UploadResponse {
            job_id,
            message: "Processing in background".into(),
        }),
    )
}

pub(crate) async fn serve_video(
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

    let job_id = vals[0];

    let path = PathBuf::from(crate::VIDEOS_DIR)
        .join(job_id)
        .join(&filename);
    debug!(%job_id, %filename, ?path, "Request server file");

    let Ok(mut fh) = tokio::fs::File::open(&path).await else {
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("File not found"))
            .unwrap());
    };
    let Ok(metadata) = fh.metadata().await else {
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("File metadata not found"))
            .unwrap());
    };

    let size = metadata.len();

    let (status, start, end) = parse_range(&req, size);
    fh.seek(std::io::SeekFrom::Start(start)).await.unwrap(); // seek to start, just panic if failed
    let len = end - start + 1;

    let bucket = Arc::new(TokioMutex::new(TokenBucket::new(
        RATE_BYTES_PER_SEC,
        RATE_BYTES_PER_SEC,
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
    let upload_dir = PathBuf::from(crate::VIDEOS_DIR)
        .join(&job_id)
        .join(crate::VIDEO_OBJECTS_DIR);
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
    AxumPath((job_id, filename)): AxumPath<(String, String)>,
    req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    let path = PathBuf::from(crate::VIDEOS_DIR)
        .join(&job_id)
        .join(crate::VIDEO_OBJECTS_DIR)
        .join(&filename);
    debug!(%job_id, %filename, ?path, "Request server file");

    let Ok(mut fh) = tokio::fs::File::open(&path).await else {
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("File not found"))
            .unwrap());
    };
    let Ok(metadata) = fh.metadata().await else {
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("File metadata not found"))
            .unwrap());
    };

    let size = metadata.len();

    let (status, start, end) = parse_range(&req, size);
    fh.seek(std::io::SeekFrom::Start(start)).await.unwrap(); // seek to start, just panic if failed
    let len = end - start + 1;

    let bucket = Arc::new(TokioMutex::new(TokenBucket::new(
        RATE_BYTES_PER_SEC,
        RATE_BYTES_PER_SEC,
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
