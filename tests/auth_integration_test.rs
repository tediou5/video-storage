use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use video_storage::Config;

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct CreateClaimRequest {
    pub asset_id: String,
    pub nbf_unix: Option<u32>,
    pub exp_unix: u32,
    #[serde(default)]
    pub window_len_sec: Option<u16>,
    #[serde(default)]
    pub max_kbps: Option<u16>,
    #[serde(default)]
    pub max_concurrency: Option<u16>,
    #[serde(default)]
    pub allowed_widths: Option<Vec<u16>>,
}

#[derive(serde::Deserialize)]
struct CreateClaimResponse {
    pub token: String,
}

/// Test harness that manages the server process
struct TestServer {
    handle: JoinHandle<()>,
    e_port: u16,
    i_port: u16,
    workspace: String,
    client: reqwest::Client,
}

impl TestServer {
    /// Start the server in a subprocess
    async fn start() -> Self {
        // Only open when debugging
        // tracing_subscriber::fmt::init();

        // Find an available port
        let e_port = portpicker::pick_unused_port().expect("No available port");
        let i_port = portpicker::pick_unused_port().expect("No available port");

        let test_id = uuid::Uuid::new_v4().to_string();
        let workspace = format!("/tmp/test-workspace-{test_id}");

        let config = Config {
            listen_on_port: e_port,
            internal_port: i_port,
            workspace: workspace.clone(),
            ..Default::default()
        };

        let handle = tokio::spawn(async move {
            video_storage::run(config).await;
        });

        // Wait for server to be ready
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();

        sleep(Duration::from_millis(1)).await;
        // Poll until server is ready
        for _ in 0..3 {
            if let Ok(response) = client
                .get(format!("http://127.0.0.1:{i_port}/waitlist"))
                .send()
                .await
                && response.status().is_success()
            {
                break;
            }

            sleep(Duration::from_millis(10)).await;
        }

        TestServer {
            handle,
            e_port,
            i_port,
            client,
            workspace,
        }
    }

    fn ext_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.e_port)
    }

    fn int_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.i_port)
    }

    /// Create a claim token via HTTP API
    async fn create_claim(
        &self,
        asset_id: &str,
        allowed_widths: Vec<u16>,
        exp_offset: i64,
    ) -> Result<String, Box<dyn std::error::Error>> {
        self.create_claim_with_concurrency(asset_id, Some(allowed_widths), exp_offset, Some(3))
            .await
    }

    /// Create a claim token via HTTP API
    async fn create_claim_with_concurrency(
        &self,
        asset_id: &str,
        allowed_widths: Option<Vec<u16>>,
        exp_offset: i64,
        max_concurrency: Option<u16>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;

        let request = CreateClaimRequest {
            asset_id: asset_id.to_string(),
            nbf_unix: Some(now - 1), // Valid from 1 minute ago
            exp_unix: (now as i64 + exp_offset) as u32,
            window_len_sec: Some(300),
            max_kbps: Some(5000),
            max_concurrency,
            allowed_widths,
        };

        let url = format!("{}/claims", self.int_url());
        let response = self.client.post(&url).json(&request).send().await?;

        if !response.status().is_success() {
            return Err(format!(
                "Failed to create claim: {}, url: {url}, request: {request:?}",
                response.status(),
            )
            .into());
        }

        let claim_response: CreateClaimResponse = response.json().await?;
        Ok(claim_response.token)
    }

    /// Make an authenticated GET request
    async fn get_with_auth(&self, path: &str, token: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{}", self.ext_url(), path))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .unwrap()
    }

    /// Make an unauthenticated GET request
    async fn get_without_auth(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{}", self.int_url(), path))
            .send()
            .await
            .unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Kill the server process
        self.handle.abort();

        // Clean up test workspace
        std::fs::remove_dir_all(&self.workspace).ok();
    }
}

#[tokio::test]
async fn test_server_starts_successfully() {
    let server = TestServer::start().await;

    // Check that waitlist endpoint works
    let response = server.get_without_auth("/waitlist").await;
    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body.get("pending_convert_jobs").is_some());
    assert!(body.get("pending_upload_jobs").is_some());
    assert!(body.get("total_pending_jobs").is_some());
}

#[tokio::test]
async fn test_no_auth_fails() {
    let server = TestServer::start().await;

    // Request from internal URL should fail
    let response = server.get_without_auth("/videos/test_video.m3u8").await;
    assert_eq!(response.status(), 404);

    // Request without Authorization header should fail
    let response = server
        .client
        .get(format!("{}/videos/test_video.m3u8", server.ext_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    // Request with invalid Authorization header should fail
    let response = server
        .client
        .get(format!("{}/videos/test_video.m3u8", server.ext_url()))
        .header("Authorization", "InvalidToken")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn test_valid_auth_succeeds() {
    let server = TestServer::start().await;

    // Create a valid claim
    let token = server
        .create_claim("test_video", vec![], 3600)
        .await
        .expect("Failed to create claim");

    // Request with valid token should succeed (would return NOT_FOUND since file doesn't exist)
    let response = server
        .get_with_auth("/videos/test_video.m3u8", &token)
        .await;
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn test_asset_id_mismatch() {
    let server = TestServer::start().await;

    // Create a claim for video1
    let token = server
        .create_claim("video1", vec![], 3600)
        .await
        .expect("Failed to create claim");

    // Try to access video2 with video1's token
    let response = server.get_with_auth("/videos/video2.m3u8", &token).await;
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn test_expired_token() {
    let server = TestServer::start().await;

    // Create an expired claim (expired 1 hour ago)
    let token = server
        .create_claim("test_video", vec![], 1)
        .await
        .expect("Failed to create claim");

    sleep(Duration::from_secs(2)).await;

    // Request with expired token should fail
    let response = server
        .get_with_auth("/videos/test_video.m3u8", &token)
        .await;
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn test_width_restrictions() {
    let server = TestServer::start().await;

    // Create a claim that only allows 720 and 1080 widths
    let token = server
        .create_claim("test_video", vec![720, 1080], 3600)
        .await
        .expect("Failed to create claim");

    // Accessing allowed width should succeed (would return NOT_FOUND since file doesn't exist)
    let response = server
        .get_with_auth("/videos/720/test_video.m3u8", &token)
        .await;
    assert_eq!(response.status(), 404);

    // Accessing disallowed width should fail
    let response = server
        .get_with_auth("/videos/1920/test_video.m3u8", &token)
        .await;
    assert_eq!(response.status(), 403);

    // Master playlist (no width) should always be allowed
    let response = server
        .get_with_auth("/videos/test_video.m3u8", &token)
        .await;
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn test_empty_allowed_widths_allows_all() {
    let server = TestServer::start().await;

    // Create a claim with empty allowed_widths (allows all widths)
    let token = server
        .create_claim("test_video", vec![], 3600)
        .await
        .expect("Failed to create claim");

    // Should allow any width
    for width in &[360, 480, 720, 1080, 1920, 3840] {
        let response = server
            .get_with_auth(&format!("/videos/{}/test_video.m3u8", width), &token)
            .await;
        assert_eq!(response.status(), 404);
    }
}

#[tokio::test]
async fn test_time_window_for_segments() {
    let server = TestServer::start().await;

    // Create a claim with 120 second window (30 segments at 4 seconds each)
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;

    let request = CreateClaimRequest {
        asset_id: "test_video".to_string(),
        nbf_unix: Some(now - 60),
        exp_unix: now + 3600,
        window_len_sec: Some(120), // 30 segments max
        max_kbps: Some(4000),
        max_concurrency: Some(10),
        allowed_widths: Some(vec![1920]),
    };

    let response = server
        .client
        .post(format!("{}/claims", server.int_url()))
        .json(&request)
        .send()
        .await
        .unwrap();

    let claim_response: CreateClaimResponse = response.json().await.unwrap();
    let token = claim_response.token;

    // Segment 20 should be allowed (within 30 segment window)
    let response = server
        .get_with_auth("/videos/test_video-020.m4s", &token)
        .await;
    assert_eq!(response.status(), 404);

    // Segment 40 should be denied (outside 30 segment window)
    let response = server
        .get_with_auth("/videos/test_video-040.m4s", &token)
        .await;
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn test_invalid_claim_token() {
    let server = TestServer::start().await;

    // Test with malformed token
    let response = server
        .client
        .get(format!("{}/videos/test_video.m3u8", server.ext_url()))
        .header("Authorization", "Bearer invalid_base64_token!!!")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    // Test with random base64 token
    let response = server
        .client
        .get(format!("{}/videos/test_video.m3u8", server.ext_url()))
        .header("Authorization", "Bearer YmFkX3Rva2Vu") // "bad_token" in base64
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn test_width_restrictions_with_segments() {
    let server = TestServer::start().await;

    // Create a claim that only allows 720 width
    let token = server
        .create_claim("test_video", vec![720], 3600)
        .await
        .expect("Failed to create claim");

    // Init segment with allowed width should pass
    let response = server
        .get_with_auth("/videos/720/test_video-init.mp4", &token)
        .await;
    assert_eq!(response.status(), 404);

    // Video segment with allowed width should pass
    let response = server
        .get_with_auth("/videos/720/test_video-001.m4s", &token)
        .await;
    assert_eq!(response.status(), 404);

    // Video segment with disallowed width should fail
    let response = server
        .get_with_auth("/videos/1080/test_video-001.m4s", &token)
        .await;
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn test_multiple_width_restrictions() {
    let server = TestServer::start().await;

    // Create a claim that allows multiple specific widths
    let token = server
        .create_claim("test_video", vec![480, 720, 1080], 3600)
        .await
        .expect("Failed to create claim");

    // Test allowed widths
    for width in &[480, 720, 1080] {
        let response = server
            .get_with_auth(&format!("/videos/{}/test_video.m3u8", width), &token)
            .await;
        assert_eq!(response.status(), 404);
    }

    // Test disallowed widths
    for width in &[360, 1920, 3840] {
        let response = server
            .get_with_auth(&format!("/videos/{}/test_video.m3u8", width), &token)
            .await;
        assert_eq!(response.status(), 403);
    }
}

#[tokio::test]
async fn test_concurrent_requests() {
    let server = TestServer::start().await;

    // Create a valid claim
    let token = server
        .create_claim_with_concurrency("test_video", None, 3600, None)
        .await
        .expect("Failed to create claim");

    // Make multiple concurrent requests with the same token
    let mut handles = vec![];
    for i in 0..5 {
        let base_url = server.ext_url();
        let token_clone = token.clone();
        let client = server.client.clone();

        let handle = tokio::spawn(async move {
            let response = client
                .get(format!("{}/videos/test_video-{:03}.m4s", base_url, i))
                .header("Authorization", format!("Bearer {}", token_clone))
                .send()
                .await
                .unwrap();
            response.status().as_u16()
        });

        handles.push(handle);
    }

    // Wait for all requests to complete
    let results: Vec<u16> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    // All should pass auth (return NOT_FOUND since files don't exist)
    for status in results {
        assert_eq!(status, 404);
    }
}

#[tokio::test]
async fn test_claim_creation_validation() {
    let server = TestServer::start().await;

    // Test with empty asset_id
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;

    let request = CreateClaimRequest {
        asset_id: "".to_string(), // Empty asset_id
        nbf_unix: Some(now - 60),
        exp_unix: now + 3600,
        window_len_sec: Some(300),
        max_kbps: Some(5000),
        max_concurrency: Some(3),
        allowed_widths: Some(vec![]),
    };

    let response = server
        .client
        .post(format!("{}/claims", server.int_url()))
        .json(&request)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 400);

    // Test with exp <= nbf
    let request = CreateClaimRequest {
        asset_id: "test_video".to_string(),
        nbf_unix: Some(now + 3600),
        exp_unix: now,
        window_len_sec: Some(300),
        max_kbps: Some(5000),
        max_concurrency: Some(3),
        allowed_widths: Some(vec![]),
    };

    let response = server
        .client
        .post(format!("{}/claims", server.int_url()))
        .json(&request)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn test_claim_with_optional_parameters() {
    let server = TestServer::start().await;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;

    // Test with minimal required fields only
    let minimal_request = CreateClaimRequest {
        asset_id: "test_video".to_string(),
        nbf_unix: None,
        exp_unix: now + 3600,
        window_len_sec: None,
        max_kbps: None,
        max_concurrency: None,
        allowed_widths: None,
    };

    let response = server
        .client
        .post(format!("{}/claims", server.int_url()))
        .json(&minimal_request)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let claim_response: CreateClaimResponse = response.json().await.unwrap();
    assert!(!claim_response.token.is_empty());

    // Test with some optional parameters
    let partial_request = CreateClaimRequest {
        asset_id: "test_video2".to_string(),
        nbf_unix: None,
        exp_unix: now + 7200,
        window_len_sec: Some(300),
        max_kbps: None,
        max_concurrency: Some(5),
        allowed_widths: None,
    };

    let response = server
        .client
        .post(format!("{}/claims", server.int_url()))
        .json(&partial_request)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let claim_response: CreateClaimResponse = response.json().await.unwrap();
    assert!(!claim_response.token.is_empty());
}
