use axum::http::StatusCode;
use thiserror::Error;

/// Claim-related errors with API error codes
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ClaimError {
    #[error("Invalid token format")]
    InvalidToken,

    #[error("Unsupported claim version {0}")]
    UnsupportedVersion(u8),

    #[error("Token has expired")]
    TokenExpired,

    #[error("Token is not yet valid")]
    TokenNotYetValid,

    #[error("AEAD decryption failed")]
    AeadFail,

    #[error("Asset ID mismatch")]
    AssetMismatch,

    #[error("Time window exceeded")]
    TimeWindowDeny,

    #[error("Key not found: {0}")]
    KeyNotFound(u8),

    #[error("Invalid header: {0}")]
    InvalidHeader(String),

    #[error("Invalid payload: {0}")]
    InvalidPayload(String),
}

impl ClaimError {
    /// Convert error to HTTP status code and API error code string
    pub fn to_err_code(&self) -> StatusCode {
        match self {
            ClaimError::InvalidToken
            | ClaimError::UnsupportedVersion(_)
            | ClaimError::TokenExpired
            | ClaimError::TokenNotYetValid
            | ClaimError::AeadFail
            | ClaimError::KeyNotFound(_)
            | ClaimError::InvalidHeader(_)
            | ClaimError::InvalidPayload(_) => StatusCode::UNAUTHORIZED,
            ClaimError::AssetMismatch | ClaimError::TimeWindowDeny => StatusCode::FORBIDDEN,
        }
    }
}
