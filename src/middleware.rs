use axum::{body::Body, http::Request, middleware::Next, response::Response};
use tracing::{error, warn};

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
