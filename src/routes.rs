use crate::{AppState, JobTask, TokenBucket, UploadParams, UploadResponse, convert_to_hls};

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
use tokio::{fs::create_dir_all, io::AsyncSeekExt, sync::Mutex as TokioMutex, task::JoinHandle};
use tokio_util::io::ReaderStream;
use tracing::error;

const JOB_FILE: &str = "jobs.json";
/// bytes per second (500 KB/s)
const RATE_BYTES_PER_SEC: f64 = 500.0 * 1024.0;

pub(crate) struct JobGenerator {
    id: String,
    upload_path: PathBuf,
    state: Arc<AppState>,
}

impl JobGenerator {
    pub(crate) fn new(id: String, upload_path: PathBuf, state: Arc<AppState>) -> Self {
        Self {
            id,
            upload_path,
            state,
        }
    }

    pub(crate) fn gen_task(&self) -> JoinHandle<anyhow::Result<()>> {
        let state = self.state.clone();
        let upload_path = self.upload_path.clone();
        let job_id = self.id.clone();
        tokio::spawn(async move {
            spawn_hls_job(state, upload_path, job_id.clone())
                .await
                .inspect_err(|error| error!(%job_id, %error, "Failed to process video"))
        })
    }
}

pub(crate) async fn upload_mp4_raw(
    Extension(state): Extension<Arc<AppState>>,
    Query(UploadParams { filename }): Query<UploadParams>,
    body: Bytes,
) -> impl IntoResponse {
    let upload_dir = PathBuf::from("uploads");
    create_dir_all(&upload_dir).await.unwrap();

    let upload_path = upload_dir.join(&filename);
    if let Err(error) = tokio::fs::write(&upload_path, &*body).await {
        error!(%error, "Failed to write upload file");
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                job_id: filename.clone(),
                message: format!("Failed to upload file: {error}"),
            }),
        );
    };

    if state.job_set.lock().await.contains(&filename) {
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                job_id: filename.clone(),
                message: "already in-progress".into(),
            }),
        );
    }

    let mut jobs = state.job_set.lock().await;
    jobs.insert(filename.clone());
    let dump = serde_json::to_string(&*jobs).unwrap();
    tokio::fs::write(JOB_FILE, dump).await.unwrap();

    let generator = JobGenerator::new(filename.clone(), upload_path, state.clone());

    _ = state.job_tx.send(JobTask {
        id: filename.clone(),
        generator,
    });

    (
        StatusCode::ACCEPTED,
        Json(UploadResponse {
            job_id: filename,
            message: "Processing in background".into(),
        }),
    )
}

pub(crate) async fn spawn_hls_job(
    state: Arc<AppState>,
    upload_path: PathBuf,
    job_id: String,
) -> anyhow::Result<()> {
    let temp_dir = PathBuf::from("/tmp").join(format!("tmp-{job_id}"));
    let out_dir = PathBuf::from("videos").join(&job_id);
    create_dir_all(&temp_dir).await?;

    let job_id_c = job_id.clone();
    let input_path = upload_path.clone();
    tokio::task::spawn_blocking(move || {
        convert_to_hls(&job_id_c, &input_path, &temp_dir)?;
        std::fs::create_dir_all("videos")?;
        std::fs::rename(&temp_dir, &out_dir)?;
        Ok::<(), anyhow::Error>(())
    })
    .await??;

    // clean up upload file & update job status
    let mut js = state.job_set.lock().await;
    if js.remove(&job_id) {
        let dump = serde_json::to_string(&*js)?;
        tokio::fs::write(JOB_FILE, dump).await?;
        // remove upload file
        tokio::fs::remove_file(&upload_path).await?;
    }

    state.job_set.lock().await.remove(&job_id);

    Ok(())
}

pub(crate) async fn serve_video(
    AxumPath(file): AxumPath<String>,
    req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    let path = PathBuf::from("videos").join(&file);
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
