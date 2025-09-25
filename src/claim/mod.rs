pub mod bucket;
pub mod create_request;
pub mod error;
pub mod manager;
pub mod middleware;
pub mod payload_v1;
pub mod validation;

// Internal modules
mod header;

// Re-export public types and functions
pub use bucket::{ClaimBucket, ClaimBucketManager, ClaimBucketStats};
pub use create_request::{CreateClaimRequest, CreateClaimResponse};
pub use error::ClaimError;
pub use manager::ClaimManager;
pub use middleware::{ClaimState, claim_auth_middleware};
pub use payload_v1::ClaimPayloadV1;
pub use validation::{HLS_SEGMENT_DURATION, validate_claim_time_and_resource};
