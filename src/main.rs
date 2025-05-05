use std::{
    collections::{BTreeSet, HashMap},
    convert::Infallible,
    io::Error as IoError,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::anyhow;
use axum::{
    Router,
    body::Body,
    extract::{Extension, Path as AxumPath, Query},
    http::{Request, Response, StatusCode, header},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use axum_server::tls_rustls::RustlsConfig;
use bytes::Bytes;
use ffmpeg_next::{self as ffmpeg};
use futures::{StreamExt, TryStreamExt};
use mime_guess::from_path;
use serde::{Deserialize, Serialize};
use tokio::{
    fs::create_dir_all,
    io::AsyncSeekExt,
    sync::Mutex as TokioMutex,
    task::JoinHandle,
    time::{Instant, sleep},
};
use tokio_util::io::ReaderStream;
use tracing::error;

const JOB_FILE: &str = "jobs.json";
/// bytes per second (500 KB/s)
const RATE_BYTES_PER_SEC: f64 = 500.0 * 1024.0;

type JobTasks = TokioMutex<HashMap<String, JoinHandle<anyhow::Result<()>>>>;

struct AppState {
    job_set: TokioMutex<BTreeSet<String>>,
    job_tasks: JobTasks,
}

impl AppState {
    fn new() -> Self {
        Self {
            job_set: TokioMutex::new(BTreeSet::new()),
            job_tasks: TokioMutex::new(HashMap::new()),
        }
    }
}

/// 令牌桶限速
struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_rate: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(capacity: f64, refill_rate: f64) -> Self {
        TokenBucket {
            capacity,
            tokens: capacity,
            refill_rate,
            last: Instant::now(),
        }
    }

    async fn consume(&mut self, amount: usize) {
        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(self.last).as_secs_f64();
            self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
            self.last = now;
            if self.tokens >= amount as f64 {
                self.tokens -= amount as f64;
                break;
            }
            let need = amount as f64 - self.tokens;
            let wait = need / self.refill_rate;
            sleep(Duration::from_secs_f64(wait)).await;
        }
    }
}

#[derive(Serialize, Deserialize)]
struct UploadResponse {
    job_id: String,
    message: String,
}

#[derive(Deserialize)]
struct UploadParams {
    /// ?filename=your_video.mp4
    filename: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    ffmpeg::init().unwrap();
    let tls = RustlsConfig::from_pem_file("cert.pem", "key.pem")
        .await
        .expect("Load TLS failed");
    let state = Arc::new(AppState::new());

    // load pending jobs from file
    let json = tokio::fs::read_to_string(JOB_FILE).await.unwrap();
    let pending = serde_json::from_str::<BTreeSet<String>>(&json).unwrap();

    // TODO: handle pending jobs, e.g. restart failed jobs
    let _state_c = state.clone();
    tokio::spawn(async move { loop {} });

    for filename in pending {
        let upload_path = PathBuf::from("uploads").join(&filename);
        let st = state.clone();
        let job_id = filename.clone();
        let handle = tokio::spawn(async move {
            spawn_hls_job(st.clone(), upload_path, job_id.clone())
                .await
                .inspect_err(|error| error!(%error, %job_id, "Failed to recover pending jobs"))
        });

        state.job_tasks.lock().await.insert(filename, handle);
    }

    let app = Router::new()
        .route("/upload", post(upload_mp4_raw))
        .route("/videos/:file", get(serve_video))
        .layer(Extension(state));

    println!("Listening on https://0.0.0.0:8443");
    axum_server::bind_rustls(SocketAddr::from(([0u8, 0, 0, 0], 32145)), tls)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn upload_mp4_raw(
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

    let st = state.clone();
    let up = upload_path.clone();
    let job_id_c = filename.clone();
    let handle = tokio::spawn(async move {
        spawn_hls_job(st, up, job_id_c.clone())
            .await
            .inspect_err(|error| error!(job_id = %job_id_c, %error, "Failed to process video"))
    });
    state
        .job_tasks
        .lock()
        .await
        .insert(filename.clone(), handle);

    (
        StatusCode::ACCEPTED,
        Json(UploadResponse {
            job_id: filename,
            message: "Processing in background".into(),
        }),
    )
}

async fn spawn_hls_job(
    state: Arc<AppState>,
    upload_path: PathBuf,
    job_id: String,
) -> anyhow::Result<()> {
    let temp_dir = PathBuf::from("/tmp").join(format!("tmp-{job_id}"));
    let out_dir = PathBuf::from("videos").join(&job_id);
    create_dir_all(&temp_dir).await?;

    let input_path = upload_path.clone();
    tokio::task::spawn_blocking(move || {
        process_mp4_to_hls(&input_path, &temp_dir)?;
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

    state.job_tasks.lock().await.remove(&job_id);

    Ok(())
}

fn process_mp4_to_hls(input: &Path, out_dir: &Path) -> anyhow::Result<()> {
    use ffmpeg_next::{Dictionary, format};

    // input context
    let input_path = input.to_str().ok_or(anyhow!("Empty input path"))?;
    let mut ictx = format::input(&input_path)?;

    // output context
    let playlist_path = out_dir.join("playlist.m3u8");
    let mut octx = format::output_as(playlist_path.to_str().unwrap(), "hls")?;

    // set HLS options
    let mut opts = Dictionary::new();
    opts.set("hls_time", "4");
    opts.set("hls_playlist_type", "vod");
    opts.set(
        "hls_segment_filename",
        &format!(
            "{}/segment_%03d.ts",
            out_dir.to_str().ok_or(anyhow!("Empty output path"))?
        ),
    );
    octx.set_metadata(opts);

    // codec copy
    for stream in ictx.streams() {
        let codec_id = stream.parameters().id();
        let mut ost = octx.add_stream(codec_id)?;
        ost.set_parameters(stream.parameters());
    }

    octx.write_header()?;

    for (stream, mut packet) in ictx.packets() {
        packet.set_stream(stream.index());
        if let Some(octx_stream) = octx.stream(stream.index()) {
            // rescale timestamp
            packet.rescale_ts(stream.time_base(), octx_stream.time_base());
        }
        packet.write_interleaved(&mut octx)?;
    }
    octx.write_trailer()?;
    Ok(())
}

async fn serve_video(
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
