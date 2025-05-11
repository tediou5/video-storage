mod app_state;
use app_state::AppState;

mod job;
use job::{JOB_FILE, Job, JobGenerator};

mod routes;
use routes::{serve_video, spawn_hls_job, upload_mp4_raw};

mod stream_map;
use stream_map::StreamMap;

mod token_bucket;
use token_bucket::TokenBucket;

mod utils;
use utils::convert_to_hls;

use std::{collections::BTreeSet, path::PathBuf};

use axum::{
    Router,
    extract::Extension,
    routing::{get, post},
};
use ffmpeg_next::{self as ffmpeg};
use tokio::net::TcpListener;
use tower_http::cors::Any;
use tower_http::cors::CorsLayer;
use tracing::info;

const UPLOADS_DIR: &str = "uploads";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    ffmpeg::init().unwrap();
    let state = AppState::new();

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
        .route("/videos/{filename}", get(serve_video))
        .layer(cors)
        .layer(Extension(state));

    info!("Listening on https://0.0.0.0:32145");
    axum::serve(TcpListener::bind("0.0.0.0:32145").await.unwrap(), app)
        .await
        .unwrap();
}
