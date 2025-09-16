#![feature(atomic_try_update)]

pub mod app_state;
pub mod bucket;
pub mod claim;
pub mod claim_bucket;
pub mod claim_middleware;
pub mod config;
pub mod job;
pub mod middleware;
pub mod opendal;
pub mod routes;
pub mod stream_map;
pub mod token_bucket;
pub mod utils;

use axum::{
    Router,
    extract::Extension,
    routing::{get, post},
};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::str::FromStr;
use tokio::net::TcpListener;
use tower_http::cors::Any;
use tower_http::cors::CorsLayer;
use tracing::info;

//
// Re-export
//
pub use app_state::AppState;
pub use claim::{
    ClaimManager, ClaimPayloadV1, CreateClaimRequest, CreateClaimResponse, HLS_SEGMENT_DURATION,
    validate_claim_time_and_resource,
};
pub use claim_bucket::ClaimBucketManager;
pub use claim_middleware::{ClaimState, claim_auth_middleware};
pub use config::Config;
pub use job::{ConvertJob, JobGenerator, UploadJob};
pub use opendal::{StorageBackend, StorageConfig, StorageManager};
pub use routes::{
    create_claim, serve_video, serve_video_object, upload_files, upload_mp4_raw, waitlist,
};
pub use stream_map::StreamMap;
pub use token_bucket::TokenBucket;
pub use utils::spawn_hls_job;

pub const RESOLUTIONS: [(u32, u32); 3] = [(720, 1280), (540, 960), (480, 854)];
pub const BANDWIDTHS: [u32; 3] = [2500000, 1500000, 1000000];

pub async fn run(config: Config) {
    // Extract configuration values
    let listen_on_port = config.listen_on_port;
    let internal_port = config.internal_port;
    let permits = config.permits;
    let token_rate = config.token_rate;
    let workspace = config.workspace.clone();
    let webhook_url = config.webhook_url.clone();
    let claim_keys = config.claim_keys.clone();

    // Parse workspace path
    let workspace_path = PathBuf::from_str(&workspace).expect("Failed to parse workspace dir");

    // Configure storage backend
    let storage_backend = match config.storage_backend.as_str() {
        "local" => {
            info!("Using local filesystem storage");
            StorageBackend::Local
        }
        "s3" => {
            info!("Using S3 storage backend");
            let s3_config = config
                .to_s3_config()
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
        claim_keys,
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

    // Create claim state for middleware
    let claim_state = claim_middleware::ClaimState {
        claim_manager: state.claim_manager.clone(),
        bucket_manager: state.claim_bucket_manager.clone(),
    };

    // External routes (claim-protected)
    let external_app = Router::new()
        .route("/videos/{*filename}", get(serve_video))
        .route_layer(axum::middleware::from_fn_with_state(
            claim_state,
            claim_middleware::claim_auth_middleware,
        ))
        .layer(axum::middleware::from_fn(middleware::log_request_errors))
        .layer(cors.clone())
        .layer(Extension(state.clone()));

    // Internal routes (no claim needed)
    let internal_app = Router::new()
        .route("/claims", post(create_claim))
        .route("/upload", post(upload_mp4_raw))
        .route("/upload-objects", post(upload_files))
        .route("/objects/{job_id}/{filename}", get(serve_video_object))
        .route("/waitlist", get(waitlist))
        .layer(axum::middleware::from_fn(middleware::log_request_errors))
        .layer(cors)
        .layer(Extension(state));

    // Start external API server
    let external_addr = format!("0.0.0.0:{listen_on_port}");
    info!("External API listening on {external_addr}");
    let external_listener = TcpListener::bind(&external_addr)
        .await
        .expect("Failed to bind external API");

    // Start internal API server
    let internal_addr = format!("0.0.0.0:{internal_port}");
    info!("Internal API listening on {internal_addr}");
    let internal_listener = TcpListener::bind(&internal_addr)
        .await
        .expect("Failed to bind internal API");

    // Run both servers concurrently
    tokio::select! {
        result = axum::serve(external_listener, external_app) => {
            result.expect("External API server error");
        }
        result = axum::serve(internal_listener, internal_app) => {
            result.expect("Internal API server error");
        }
    }
}
