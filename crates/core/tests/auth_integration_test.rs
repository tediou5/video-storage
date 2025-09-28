use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::time::sleep;
use video_storage_test_server::{CreateClaimRequest, CreateClaimResponse, TestServer};

#[tokio::test]
async fn test_server_starts_successfully() {
    let server = TestServer::shared().await;
    let client = server.client();

    // Check that waitlist endpoint works
    let response = server.get_without_auth(&client, "/waitlist").await;
    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body.get("pending_convert_jobs").is_some());
    assert!(body.get("pending_upload_jobs").is_some());
    assert!(body.get("total_pending_jobs").is_some());
}

#[tokio::test]
async fn test_no_auth_fails() {
    let server = TestServer::shared().await;
    let client = server.client();

    // Request from internal URL should fail
    let response = server
        .get_without_auth(&client, "/videos/test_video.m3u8")
        .await;
    assert_eq!(response.status(), 404);

    // Request without Authorization header should fail
    let response = client
        .get(format!("{}/videos/test_video.m3u8", server.ext_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    // Request with invalid Authorization header should fail
    let response = client
        .get(format!("{}/videos/test_video.m3u8", server.ext_url()))
        .header("Authorization", "InvalidToken")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn test_valid_auth_succeeds() {
    let server = TestServer::shared().await;
    let client = server.client();

    // Create a valid claim
    let token = server
        .create_claim(&client, "test_video", vec![], 3600)
        .await
        .expect("Failed to create claim");

    // Request with valid token should succeed (would return NOT_FOUND since file doesn't exist)
    let response = server
        .get_with_auth(&client, "/videos/test_video.m3u8", &token)
        .await;
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn test_asset_id_mismatch() {
    let server = TestServer::shared().await;
    let client = server.client();

    // Create a claim for video1
    let token = server
        .create_claim(&client, "video1", vec![], 3600)
        .await
        .expect("Failed to create claim");

    // Try to access video2 with video1's token
    let response = server
        .get_with_auth(&client, "/videos/video2.m3u8", &token)
        .await;
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn test_expired_token() {
    let server = TestServer::shared().await;
    let client = server.client();

    // Create an expired claim (expired 1 hour ago)
    let token = server
        .create_claim(&client, "test_video", vec![], 1)
        .await
        .expect("Failed to create claim");

    sleep(Duration::from_secs(2)).await;

    // Request with expired token should fail
    let response = server
        .get_with_auth(&client, "/videos/test_video.m3u8", &token)
        .await;
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn test_width_restrictions() {
    let server = TestServer::shared().await;
    let client = server.client();

    // Create a claim that only allows 720 and 1080 widths
    let token = server
        .create_claim(&client, "test_video", vec![720, 1080], 3600)
        .await
        .expect("Failed to create claim");

    // Accessing allowed width should succeed (would return NOT_FOUND since file doesn't exist)
    let response = server
        .get_with_auth(&client, "/videos/720/test_video.m3u8", &token)
        .await;
    assert_eq!(response.status(), 404);

    // Accessing disallowed width should fail
    let response = server
        .get_with_auth(&client, "/videos/1920/test_video.m3u8", &token)
        .await;
    assert_eq!(response.status(), 403);

    // Master playlist (no width) should always be allowed
    let response = server
        .get_with_auth(&client, "/videos/test_video.m3u8", &token)
        .await;
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn test_empty_allowed_widths_allows_all() {
    let server = TestServer::shared().await;
    let client = server.client();

    // Create a claim with empty allowed_widths (allows all widths)
    let token = server
        .create_claim(&client, "test_video", vec![], 3600)
        .await
        .expect("Failed to create claim");

    // Should allow any width
    for width in &[360, 480, 720, 1080, 1920, 3840] {
        let response = server
            .get_with_auth(
                &client,
                &format!("/videos/{}/test_video.m3u8", width),
                &token,
            )
            .await;
        assert_eq!(response.status(), 404);
    }
}

#[tokio::test]
async fn test_time_window_for_segments() {
    let server = TestServer::shared().await;
    let client = server.client();

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

    let response = client
        .post(format!("{}/claims", server.int_url()))
        .json(&request)
        .send()
        .await
        .unwrap();

    let claim_response: CreateClaimResponse = response.json().await.unwrap();
    let token = claim_response.token;

    // Segment 20 should be allowed (within 30 segment window)
    let response = server
        .get_with_auth(&client, "/videos/test_video-020.m4s", &token)
        .await;
    assert_eq!(response.status(), 404);

    // Segment 40 should be denied (outside 30 segment window)
    let response = server
        .get_with_auth(&client, "/videos/test_video-040.m4s", &token)
        .await;
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn test_invalid_claim_token() {
    let server = TestServer::shared().await;
    let client = server.client();

    // Test with malformed token
    let response = client
        .get(format!("{}/videos/test_video.m3u8", server.ext_url()))
        .header("Authorization", "Bearer invalid_base64_token!!!")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    // Test with random base64 token
    let response = client
        .get(format!("{}/videos/test_video.m3u8", server.ext_url()))
        .header("Authorization", "Bearer YmFkX3Rva2Vu") // "bad_token" in base64
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn test_width_restrictions_with_segments() {
    let server = TestServer::shared().await;
    let client = server.client();

    // Create a claim that only allows 720 width
    let token = server
        .create_claim(&client, "test_video", vec![720], 3600)
        .await
        .expect("Failed to create claim");

    // Init segment with allowed width should pass
    let response = server
        .get_with_auth(&client, "/videos/720/test_video-init.mp4", &token)
        .await;
    assert_eq!(response.status(), 404);

    // Video segment with allowed width should pass
    let response = server
        .get_with_auth(&client, "/videos/720/test_video-001.m4s", &token)
        .await;
    assert_eq!(response.status(), 404);

    // Video segment with disallowed width should fail
    let response = server
        .get_with_auth(&client, "/videos/1080/test_video-001.m4s", &token)
        .await;
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn test_multiple_width_restrictions() {
    let server = TestServer::shared().await;
    let client = server.client();

    // Create a claim that allows multiple specific widths
    let token = server
        .create_claim(&client, "test_video", vec![480, 720, 1080], 3600)
        .await
        .expect("Failed to create claim");

    // Test allowed widths
    for width in &[480, 720, 1080] {
        let response = server
            .get_with_auth(
                &client,
                &format!("/videos/{}/test_video.m3u8", width),
                &token,
            )
            .await;
        assert_eq!(response.status(), 404);
    }

    // Test disallowed widths
    for width in &[360, 1920, 3840] {
        let response = server
            .get_with_auth(
                &client,
                &format!("/videos/{}/test_video.m3u8", width),
                &token,
            )
            .await;
        assert_eq!(response.status(), 403);
    }
}

#[tokio::test]
async fn test_concurrent_requests() {
    let server = TestServer::shared().await;
    let client = server.client();

    // Create a valid claim
    let token = server
        .create_claim_with_concurrency(&client, "test_video", None, 3600, None)
        .await
        .expect("Failed to create claim");

    // Make multiple concurrent requests with the same token
    let mut handles = vec![];
    let client = server.client();
    for i in 0..5 {
        let base_url = server.ext_url();
        let token_clone = token.clone();
        let client_c = client.clone();
        let handle = tokio::spawn(async move {
            let response = client_c
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
    let server = TestServer::shared().await;
    let client = server.client();

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

    let response = client
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

    let response = client
        .post(format!("{}/claims", server.int_url()))
        .json(&request)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn test_claim_with_optional_parameters() {
    let server = TestServer::shared().await;
    let client = server.client();

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

    let response = client
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

    let response = client
        .post(format!("{}/claims", server.int_url()))
        .json(&partial_request)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let claim_response: CreateClaimResponse = response.json().await.unwrap();
    assert!(!claim_response.token.is_empty());
}
