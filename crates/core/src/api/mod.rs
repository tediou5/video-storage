pub mod middleware;
pub mod routes;

// Re-export public types and functions
pub use middleware::{log_request_errors, no_auth_middleware};
pub use routes::{create_claim, migrate_videos, serve_video, upload_mp4_raw, waitlist};
