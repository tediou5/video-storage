mod app_state;
mod bucket;
use app_state::AppState;

mod job;
use job::{JOB_FILE, Job, JobGenerator};

mod middleware;

mod routes;
use routes::{serve_video, serve_video_object, upload_files, upload_mp4_raw};

mod stream_map;
use stream_map::StreamMap;

mod token_bucket;
use token_bucket::TokenBucket;

mod utils;
use utils::spawn_hls_job;

use std::{collections::BTreeSet, path::PathBuf};

use axum::{
    Router,
    extract::Extension,
    routing::{get, post},
};
use clap::Parser;
use ffmpeg_next::{self as ffmpeg};
use tokio::net::TcpListener;
use tower_http::cors::Any;
use tower_http::cors::CorsLayer;
use tracing::info;

const TEMP_DIR: &str = "temp";
const UPLOADS_DIR: &str = "uploads";
const VIDEOS_DIR: &str = "videos";
const VIDEO_OBJECTS_DIR: &str = "video-objects";
const RESOLUTIONS: [(u32, u32); 3] = [(720, 1280), (540, 960), (480, 854)];
const BANDWIDTHS: [u32; 3] = [2500000, 1500000, 1000000];

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value_t = 32145)]
    listen_on_port: u16,
    #[arg(short, long, default_value_t = 5)]
    permits: usize,
}

#[tokio::main]
async fn main() {
    let Args {
        listen_on_port,
        permits,
    } = Args::parse();

    tracing_subscriber::fmt::init();
    ffmpeg::init().unwrap();
    let state = AppState::new(permits);

    // load pending jobs from file
    let json = tokio::fs::read_to_string(JOB_FILE)
        .await
        .unwrap_or_else(|error| {
            info!(%error, "No jobs file, creating new one");
            _ = std::fs::File::create(JOB_FILE).expect("Failed to create jobs file");
            String::new()
        });

    let pending = serde_json::from_str::<BTreeSet<Job>>(&json).unwrap_or_default();
    for job in pending {
        info!(job_id = %job.id(), "Loading pending job");
        let upload_path = PathBuf::from("uploads").join(job.id());
        let generator = JobGenerator::new(job, upload_path);
        _ = state.job_tx.unbounded_send(generator);
    }

    // 创建 CORS layer
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/upload", post(upload_mp4_raw))
        .route("/videos/{*filename}", get(serve_video))
        .route("/upload-objects", post(upload_files))
        .route("/objects/{job_id}/{filename}", get(serve_video_object))
        .layer(axum::middleware::from_fn(middleware::log_request_errors))
        .layer(cors)
        .layer(Extension(state));

    let listen_on = format!("0.0.0.0:{listen_on_port}");
    info!("Listening on {listen_on}");
    axum::serve(TcpListener::bind(listen_on).await.unwrap(), app)
        .await
        .unwrap();
}
