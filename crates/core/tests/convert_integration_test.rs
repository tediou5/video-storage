use serde_json::Number as JsonNumber;
use serde_json::Value as JsonValue;
use video_storage_core::job::convert::{Scale, Scales};
use video_storage_test_server::TestServer;

/// Test 480p-only conversion with test.mp4 and webhook monitoring
#[tokio::test]
async fn test_480p_conversion() {
    // Use shared test server with webhook
    let server = TestServer::shared_with_webhook().await;
    let client = server.client();

    // Read the test video file
    let video_data = tokio::fs::read("test.mp4")
        .await
        .expect("Failed to read test.mp4");
    let scales = Scales::from_vec(vec![Scale::new(480, 854)]);

    let job_id = "test_480p_conversion";

    // Upload video with 480p-only scales
    let upload_response = server
        .upload_video_with_scales(&client, job_id, 23, scales, video_data)
        .await
        .expect("Failed to upload video");

    assert_eq!(upload_response.job_id, job_id);

    let response = server.get_without_auth(&client, "/waitlist").await;
    assert_eq!(response.status(), 200);

    let body: JsonValue = response.json().await.unwrap();
    assert_eq!(
        body.get("pending_convert_jobs"),
        Some(&JsonValue::Number(JsonNumber::from(1)))
    );
    assert_eq!(
        body.get("pending_upload_jobs"),
        Some(&JsonValue::Number(JsonNumber::from(0)))
    );
    assert_eq!(
        body.get("total_pending_jobs"),
        Some(&JsonValue::Number(JsonNumber::from(1)))
    );

    // Wait for webhook callback
    let webhook = server.shared_webhook();
    let webhook_received = webhook.wait_for_calls(1, 900).await; // 15 mins
    assert!(webhook_received, "Webhook was not called within timeout");

    // Verify webhook payload
    let calls = webhook.get_calls().await;
    assert_eq!(calls.len(), 1, "Expected exactly one webhook call");

    let webhook_payload = &calls[0];
    // Verify webhook data
    assert_eq!(webhook_payload["job_id"], job_id);
    assert_eq!(webhook_payload["job_type"], "convert");
    assert_eq!(webhook_payload["status"], "completed");

    let response = server.get_without_auth(&client, "/waitlist").await;
    assert_eq!(response.status(), 200);

    let body: JsonValue = response.json().await.unwrap();
    assert_eq!(
        body.get("pending_convert_jobs"),
        Some(&JsonValue::Number(JsonNumber::from(0)))
    );
    assert_eq!(
        body.get("pending_upload_jobs"),
        Some(&JsonValue::Number(JsonNumber::from(0)))
    );
    assert_eq!(
        body.get("total_pending_jobs"),
        Some(&JsonValue::Number(JsonNumber::from(0)))
    );

    // Create a valid claim
    let token = server
        .create_claim(&client, job_id, vec![], 3600)
        .await
        .expect("Failed to create claim");

    let response = server
        .get_with_auth(&client, &format!("/videos/{job_id}.m3u8"), &token)
        .await;
    assert_eq!(response.status(), 200);

    let response = server
        .get_with_auth(&client, &format!("/videos/480/{job_id}.m3u8"), &token)
        .await;
    assert_eq!(response.status(), 200);

    let response = server
        .get_with_auth(&client, &format!("/videos/720/{job_id}.m3u8"), &token)
        .await;
    assert_eq!(response.status(), 404);
}
