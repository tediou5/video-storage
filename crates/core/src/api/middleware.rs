use axum::{body::Body, http::Request, middleware::Next, response::Response};
use tracing::{error, warn};
use video_storage_claim::ClaimBucket;

pub async fn log_request_errors(req: Request<Body>, next: Next) -> Response {
    let uri = req.uri().clone();
    let method = req.method().clone();

    let response = next.run(req).await;
    let status = response.status();
    if status.is_client_error() {
        // 4xx error
        warn!(
            method = %method,
            uri = %uri,
            status = %status,
            "Client error"
        );
    } else if status.is_server_error() {
        // 5xx error
        error!(
            method = %method,
            uri = %uri,
            status = %status,
            "Server error"
        );
    }

    response
}

/// Middleware for claim-based authentication and authorization
pub async fn no_auth_middleware(mut req: Request<Body>, next: Next) -> Response {
    let bucket = ClaimBucket::new(0.0, 0, 0, 0);

    // Store bucket in extensions for the route handler to use
    req.extensions_mut().insert(bucket);

    // Continue to the actual handler
    next.run(req).await
}
