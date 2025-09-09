mod app_state;
mod bucket;
mod config;
mod job;
mod middleware;
mod opendal;
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
use config::Config;
use ffmpeg_next::{self as ffmpeg};
use job::{ConvertJob, JobGenerator, UploadJob};
use opendal::{StorageBackend, StorageConfig, StorageManager};
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
pub struct Args {
    #[arg(short, long, default_value_t = 32145)]
    listen_on_port: u16,
    #[arg(short, long, default_value_t = 5)]
    permits: usize,
    #[arg(short, long, default_value_t = 0.0)]
    token_rate: f64,
    #[arg(short = 'w', long, default_value = ".")]
    workspace: String,

    /// Configuration file path (overrides all other arguments)
    #[arg(short, long)]
    config: Option<String>,

    /// Storage backend: local or s3
    #[arg(short, long, default_value = "local")]
    storage_backend: String,

    /// S3 bucket name (required when storage-backend is s3)
    #[arg(long)]
    s3_bucket: Option<String>,

    /// S3 endpoint (for MinIO/custom S3)
    #[arg(long)]
    s3_endpoint: Option<String>,

    /// S3 region
    #[arg(long)]
    s3_region: Option<String>,

    /// S3 access key ID
    #[arg(long)]
    s3_access_key_id: Option<String>,

    /// S3 secret access key
    #[arg(long)]
    s3_secret_access_key: Option<String>,

    /// Webhook URL to call when jobs complete
    #[arg(long)]
    webhook_url: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    tracing_subscriber::fmt::init();
    ffmpeg::init().unwrap();

    // Load configuration
    let mut config = if let Some(config_path) = &args.config {
        info!("Loading configuration from: {}", config_path);
        Config::from_file(std::path::Path::new(config_path))
            .expect("Failed to load configuration file")
    } else {
        Config::default()
    };

    // Merge CLI arguments with config (CLI takes precedence)
    config.merge_with_cli(&args);

    // Validate final configuration
    config.validate().expect("Invalid configuration");

    // Extract configuration values
    let listen_on_port = config.listen_on_port;
    let permits = config.permits;
    let token_rate = config.token_rate;
    let workspace = config.workspace.clone();
    let webhook_url = config.webhook.as_ref().map(|w| w.url.clone());

    // Parse workspace path
    let workspace_path = PathBuf::from_str(&workspace).expect("Failed to parse workspace dir");

    // Configure storage backend
    let storage_backend = match config.storage.backend.as_str() {
        "local" => {
            info!("Using local filesystem storage");
            StorageBackend::Local
        }
        "s3" => {
            info!("Using S3 storage backend");
            let s3_config = config
                .storage
                .s3
                .expect("S3 configuration is required when using S3 backend");
            StorageBackend::S3 {
                bucket: s3_config.bucket,
                endpoint: s3_config.endpoint,
                region: s3_config.region,
                access_key_id: s3_config.access_key_id,
                secret_access_key: s3_config.secret_access_key,
            }
        }
        backend => {
            panic!(
                "Unsupported storage backend: {}. Use 'local' or 's3'",
                backend
            );
        }
    };

    let storage_config = StorageConfig {
        backend: storage_backend,
        workspace: workspace_path.clone(),
    };

    let storage_manager = StorageManager::new(storage_config)
        .await
        .expect("Failed to initialize storage manager");

    let state = AppState::new(
        token_rate,
        permits,
        &workspace_path,
        storage_manager,
        webhook_url,
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

    let pending = serde_json::from_str::<BTreeSet<ConvertJob>>(&json).unwrap_or_default();
    for job in pending {
        info!(job_id = %job.id(), "Loading pending job");
        let upload_path = state.uploads_dir().join(job.id());
        let generator = JobGenerator::new(job, upload_path);
        _ = state.job_tx.unbounded_send(generator.into());
    }

    // load pending upload jobs from file
    let upload_json = tokio::fs::read_to_string(state.pending_uploads_path())
        .await
        .unwrap_or_else(|error| {
            info!(%error, "No upload jobs file, creating new one");
            _ = std::fs::File::create(state.pending_uploads_path())
                .expect("Failed to create upload jobs file");
            String::new()
        });

    let pending_uploads =
        serde_json::from_str::<BTreeSet<UploadJob>>(&upload_json).unwrap_or_default();
    for upload_job in pending_uploads {
        info!(job_id = %upload_job.id(), "Loading pending upload job");
        _ = state.job_tx.unbounded_send(upload_job.into());
    }

    // CORS layer
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
