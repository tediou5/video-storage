pub mod bucket;
pub mod manager;
pub mod middleware;

// Re-export public types and functions
pub use bucket::{ClaimBucket, ClaimBucketManager, ClaimBucketStats, ConnectionGuard};
pub use manager::{
    ClaimError, ClaimManager, ClaimPayloadV1, CreateClaimRequest, CreateClaimResponse,
    HLS_SEGMENT_DURATION, validate_claim_time_and_resource,
};
pub use middleware::{ClaimState, claim_auth_middleware};
