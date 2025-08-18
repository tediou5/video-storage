mod app_state;
mod bucket;
mod job;
mod middleware;
mod routes;
mod stream_map;
mod token_bucket;
mod utils;

use app_state::AppState;
use axum::{
    Router,
    extract::Extension,
    routing::{get, post},
};
use clap::Parser;
use ffmpeg_next::{self as ffmpeg};
use job::{Job, JobGenerator};
use routes::{serve_video, serve_video_object, upload_files, upload_mp4_raw, waitlist};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::str::FromStr;
use stream_map::StreamMap;
use token_bucket::TokenBucket;
use tokio::net::TcpListener;
use tower_http::cors::Any;
use tower_http::cors::CorsLayer;
use tracing::info;
use utils::spawn_hls_job;

const RESOLUTIONS: [(u32, u32); 3] = [(720, 1280), (540, 960), (480, 854)];
const BANDWIDTHS: [u32; 3] = [2500000, 1500000, 1000000];

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value_t = 32145)]
    listen_on_port: u16,
    #[arg(short, long, default_value_t = 5)]
    permits: usize,
    #[arg(short, long, default_value_t = 0.0)]
    token_rate: f64,
    #[arg(short, long, default_value = ".")]
    workspace: String,
}

#[tokio::main]
async fn main() {
    let Args {
        listen_on_port,
        permits,
        token_rate,
        workspace,
    } = Args::parse();

    tracing_subscriber::fmt::init();
    ffmpeg::init().unwrap();
    let state = AppState::new(
        token_rate,
        permits,
        &PathBuf::from_str(&workspace).expect("Failed to parse workspace dir"),
    )
    .await
    .expect("Failed to create app state");

    // load pending jobs from file
    let json = tokio::fs::read_to_string(state.pending_jobs_path())
        .await
        .unwrap_or_else(|error| {
            info!(%error, "No jobs file, creating new one");
            _ = std::fs::File::create(state.pending_jobs_path())
                .expect("Failed to create jobs file");
            String::new()
        });

    let pending = serde_json::from_str::<BTreeSet<Job>>(&json).unwrap_or_default();
    for job in pending {
        info!(job_id = %job.id(), "Loading pending job");
        let upload_path = state.uploads_dir().join(job.id());
        let generator = JobGenerator::new(job, upload_path);
        _ = state.job_tx.unbounded_send(generator);
    }

    // 创建 CORS layer
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/waitlist", get(waitlist))
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
