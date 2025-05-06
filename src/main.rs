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

use std::{collections::BTreeSet, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Router,
    extract::Extension,
    routing::{get, post},
};
use axum_server::tls_rustls::RustlsConfig;
use ffmpeg_next::{self as ffmpeg};

const JOB_FILE: &str = "jobs.json";

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

    for filename in pending {
        let upload_path = PathBuf::from("uploads").join(&filename);
        let generator = JobGenerator::new(filename.clone(), upload_path, state.clone());
        _ = state.job_tx.send(JobTask {
            id: filename,
            generator,
        });
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
