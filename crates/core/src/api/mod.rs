pub mod middleware;
pub mod routes;

// Re-export public types and functions
pub use middleware::log_request_errors;
pub use routes::{create_claim, serve_video, upload_mp4_raw, waitlist};
