#![allow(incomplete_features)]
#![feature(lazy_type_alias)]

pub mod api;
pub mod app_state;
pub mod claim;
pub mod config;
pub mod job;
pub mod opendal;
pub mod stream_map;

use axum::Router;
use axum::extract::Extension;
use axum::routing::{get, post};
use std::path::PathBuf;
use std::str::FromStr;
use tokio::net::TcpListener;
use tower_http::cors::Any;
use tower_http::cors::CorsLayer;
use tracing::info;

//
// Re-export
//
pub use api::{
    TokenBucket, create_claim, log_request_errors, serve_video, upload_mp4_raw, waitlist,
};
pub use app_state::AppState;
pub use claim::{
    ClaimBucketManager, ClaimManager, ClaimPayloadV1, ClaimState, CreateClaimRequest,
    CreateClaimResponse, HLS_SEGMENT_DURATION, claim_auth_middleware,
    validate_claim_time_and_resource,
};
pub use config::Config;
pub use job::{ConvertJob, Job, JobResult, UploadJob};
pub use opendal::{StorageBackend, StorageConfig, StorageManager};
pub use stream_map::StreamMap;

pub const RESOLUTIONS: [(u32, u32); 3] = [(720, 1280), (540, 960), (480, 854)];
pub const BANDWIDTHS: [u32; 3] = [2500000, 1500000, 1000000];

pub async fn run(config: Config) {
    // Ensure we're in a proper async context by yielding once
    tokio::task::yield_now().await;

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

    // CORS layer
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Create claim state for middleware
    let claim_state = claim::ClaimState {
        claim_manager: state.claim_manager.clone(),
        bucket_manager: state.claim_bucket_manager.clone(),
    };

    // External routes (claim-protected)
    let external_app = Router::new()
        .route("/videos/{*filename}", get(serve_video))
        .route_layer(axum::middleware::from_fn_with_state(
            claim_state,
            claim::claim_auth_middleware,
        ))
        .layer(axum::middleware::from_fn(api::log_request_errors))
        .layer(cors.clone())
        .layer(Extension(state.clone()));

    // Internal routes (no claim needed)
    let internal_app = Router::new()
        .route("/claims", post(create_claim))
        .route("/upload", post(upload_mp4_raw))
        .route("/waitlist", get(waitlist))
        .layer(axum::middleware::from_fn(api::log_request_errors))
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
