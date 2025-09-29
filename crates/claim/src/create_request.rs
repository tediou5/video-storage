use serde::{Deserialize, Serialize};

/// Request structure for creating a claim token
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateClaimRequest {
    /// Asset ID (job_id) to grant access to (required)
    pub asset_id: String,

    /// Expiration time in Unix timestamp (required)
    pub exp_unix: u32,

    /// Not-before time in Unix timestamp (optional, defaults to current time)
    pub nbf_unix: Option<u32>,

    /// Time window length in seconds (optional, defaults to 0 = unlimited)
    #[serde(default)]
    pub window_len_sec: Option<u16>,

    /// Maximum bandwidth in kbps (optional, defaults to 0 = unlimited)
    #[serde(default)]
    pub max_kbps: Option<u16>,

    /// Maximum concurrent connections (optional, defaults to 0 = unlimited)
    #[serde(default)]
    pub max_concurrency: Option<u16>,

    /// Allowed video widths/resolutions (optional, defaults to empty = all widths allowed)
    #[serde(default)]
    pub allowed_widths: Option<Vec<u16>>,
}

/// Response structure for claim creation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateClaimResponse {
    pub token: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_claim_request_json_parsing() {
        // Test with all fields
        let json_all = r#"{
            "asset_id": "video123",
            "nbf_unix": 1234567890,
            "exp_unix": 1234567900,
            "window_len_sec": 300,
            "max_kbps": 5000,
            "max_concurrency": 10,
            "allowed_widths": [1920, 1280, 854]
        }"#;

        let request: CreateClaimRequest = serde_json::from_str(json_all).unwrap();
        assert_eq!(request.asset_id, "video123");
        assert_eq!(request.nbf_unix, Some(1234567890));
        assert_eq!(request.exp_unix, 1234567900);
        assert_eq!(request.window_len_sec, Some(300));
        assert_eq!(request.max_kbps, Some(5000));
        assert_eq!(request.max_concurrency, Some(10));
        assert_eq!(request.allowed_widths, Some(vec![1920, 1280, 854]));

        // Test with minimal required fields only
        let json_minimal = r#"{
            "asset_id": "video456",
            "exp_unix": 1234567900
        }"#;

        let request: CreateClaimRequest = serde_json::from_str(json_minimal).unwrap();
        assert_eq!(request.asset_id, "video456");
        assert_eq!(request.nbf_unix, None);
        assert_eq!(request.exp_unix, 1234567900);
        assert_eq!(request.window_len_sec, None);
        assert_eq!(request.max_kbps, None);
        assert_eq!(request.max_concurrency, None);
        assert_eq!(request.allowed_widths, None);

        // Test with some optional fields
        let json_partial = r#"{
            "asset_id": "video789",
            "exp_unix": 1234567900,
            "max_kbps": 3000,
            "allowed_widths": [1920]
        }"#;

        let request: CreateClaimRequest = serde_json::from_str(json_partial).unwrap();
        assert_eq!(request.asset_id, "video789");
        assert_eq!(request.nbf_unix, None);
        assert_eq!(request.exp_unix, 1234567900);
        assert_eq!(request.window_len_sec, None);
        assert_eq!(request.max_kbps, Some(3000));
        assert_eq!(request.max_concurrency, None);
        assert_eq!(request.allowed_widths, Some(vec![1920]));

        // Test with empty arrays and zero values
        let json_empty = r#"{
            "asset_id": "video000",
            "exp_unix": 1234567900,
            "window_len_sec": 0,
            "max_kbps": 0,
            "max_concurrency": 0,
            "allowed_widths": []
        }"#;

        let request: CreateClaimRequest = serde_json::from_str(json_empty).unwrap();
        assert_eq!(request.asset_id, "video000");
        assert_eq!(request.window_len_sec, Some(0));
        assert_eq!(request.max_kbps, Some(0));
        assert_eq!(request.max_concurrency, Some(0));
        assert_eq!(request.allowed_widths, Some(vec![]));

        // Test null values are treated as None
        let json_nulls = r#"{
            "asset_id": "video_null",
            "exp_unix": 1234567900,
            "nbf_unix": null,
            "window_len_sec": null,
            "max_kbps": null,
            "max_concurrency": null,
            "allowed_widths": null
        }"#;

        let request: CreateClaimRequest = serde_json::from_str(json_nulls).unwrap();
        assert_eq!(request.asset_id, "video_null");
        assert_eq!(request.nbf_unix, None);
        assert_eq!(request.window_len_sec, None);
        assert_eq!(request.max_kbps, None);
        assert_eq!(request.max_concurrency, None);
        assert_eq!(request.allowed_widths, None);
    }

    #[test]
    fn test_create_claim_request_serialization() {
        // Test serialization with all fields
        let request = CreateClaimRequest {
            asset_id: "test_video".to_string(),
            nbf_unix: Some(1234567890),
            exp_unix: 1234567900,
            window_len_sec: Some(300),
            max_kbps: Some(5000),
            max_concurrency: Some(10),
            allowed_widths: Some(vec![1920, 1280]),
        };

        let json = serde_json::to_string(&request).unwrap();
        let parsed: CreateClaimRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.asset_id, request.asset_id);
        assert_eq!(parsed.nbf_unix, request.nbf_unix);
        assert_eq!(parsed.exp_unix, request.exp_unix);
        assert_eq!(parsed.window_len_sec, request.window_len_sec);
        assert_eq!(parsed.max_kbps, request.max_kbps);
        assert_eq!(parsed.max_concurrency, request.max_concurrency);
        assert_eq!(parsed.allowed_widths, request.allowed_widths);

        // Test serialization with minimal fields
        let minimal_request = CreateClaimRequest {
            asset_id: "minimal".to_string(),
            nbf_unix: None,
            exp_unix: 1234567900,
            window_len_sec: None,
            max_kbps: None,
            max_concurrency: None,
            allowed_widths: None,
        };

        let json = serde_json::to_string(&minimal_request).unwrap();
        assert!(json.contains("\"asset_id\":\"minimal\""));
        assert!(json.contains("\"exp_unix\":1234567900"));

        let parsed: CreateClaimRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.asset_id, minimal_request.asset_id);
        assert_eq!(parsed.exp_unix, minimal_request.exp_unix);
        assert_eq!(parsed.nbf_unix, None);
        assert_eq!(parsed.window_len_sec, None);
        assert_eq!(parsed.max_kbps, None);
        assert_eq!(parsed.max_concurrency, None);
        assert_eq!(parsed.allowed_widths, None);
    }

    #[test]
    fn test_create_claim_request_invalid_json() {
        // Missing required field asset_id
        let json_missing_asset = r#"{
            "exp_unix": 1234567900
        }"#;

        let result: Result<CreateClaimRequest, _> = serde_json::from_str(json_missing_asset);
        assert!(result.is_err());

        // Missing required field exp_unix
        let json_missing_exp = r#"{
            "asset_id": "video123"
        }"#;

        let result: Result<CreateClaimRequest, _> = serde_json::from_str(json_missing_exp);
        assert!(result.is_err());

        // Invalid type for numeric field
        let json_invalid_type = r#"{
            "asset_id": "video123",
            "exp_unix": "not_a_number"
        }"#;

        let result: Result<CreateClaimRequest, _> = serde_json::from_str(json_invalid_type);
        assert!(result.is_err());
    }
}
