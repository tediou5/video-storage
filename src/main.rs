mod app_state;
use app_state::{AppState, JobTask};

mod params;
use params::{UploadParams, UploadResponse};

mod routes;
use routes::{JobGenerator, serve_video, upload_mp4_raw};

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

const JOB_FILE: &str = "jobs.json";
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

    let pending = serde_json::from_str::<BTreeSet<String>>(&json).unwrap_or_default();
    for filename in pending {
        info!(%filename, "Loading pending job");
        let upload_path = PathBuf::from("uploads").join(&filename);
        let generator = JobGenerator::new(filename.clone(), upload_path);
        _ = state.job_tx.unbounded_send(JobTask {
            id: filename,
            generator,
        });
    }

    // 创建 CORS layer
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/upload", post(upload_mp4_raw))
        .route("/videos/{*filename}", get(serve_video))
        .layer(cors)
        .layer(Extension(state));

    info!("Listening on https://0.0.0.0:32145");
    axum::serve(TcpListener::bind("0.0.0.0:32145").await.unwrap(), app)
        .await
        .unwrap();
}
