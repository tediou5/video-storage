use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use opendal::Operator;
use video_storage_core::api::routes::{MigrateRequest, MigrateResponse};
use video_storage_core::{StorageBackend, StorageConfig, StorageManager};
use video_storage_test_server::TestServer;

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "missing env {name}. This test is #[ignore] by default; provide all VS_TEST_S3_* env vars to run it."
        )
    })
}

async fn build_operator_for_bucket(
    endpoint: Option<String>,
    region: Option<String>,
    access_key_id: String,
    secret_access_key: String,
    bucket: &str,
) -> Operator {
    let tmp = tempfile::tempdir().unwrap();
    let storage_manager = StorageManager::new(StorageConfig {
        backend: StorageBackend::S3 {
            endpoint,
            region,
            default_bucket: None,
            access_key_id,
            secret_access_key,
        },
        workspace: tmp.path().to_path_buf(),
    })
    .await
    .unwrap();
    storage_manager.operator_for_bucket(bucket).unwrap()
}

fn unique_job_id(prefix: &str) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("{prefix}{now_ms}")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_s3_serve_and_migrate_between_buckets_minio() {
    let endpoint = required_env("VS_TEST_S3_ENDPOINT");
    let region = std::env::var("VS_TEST_S3_REGION")
        .ok()
        .filter(|v| !v.is_empty());
    let access_key_id = required_env("VS_TEST_S3_ACCESS_KEY_ID");
    let secret_access_key = required_env("VS_TEST_S3_SECRET_ACCESS_KEY");
    let src_bucket = required_env("VS_TEST_S3_SRC_BUCKET");
    let dst_bucket = required_env("VS_TEST_S3_DST_BUCKET");

    let server = TestServer::start_with_config(|cfg| {
        cfg.storage_backend = "s3".into();
        cfg.s3_endpoint = Some(endpoint.clone());
        cfg.s3_region = region.clone();
        cfg.s3_bucket = Some(src_bucket.clone());
        cfg.s3_access_key_id = Some(access_key_id.clone());
        cfg.s3_secret_access_key = Some(secret_access_key.clone());
    })
    .await;

    let client = server.client();
    let job_id = unique_job_id("s3it");

    let src_op = build_operator_for_bucket(
        Some(endpoint.clone()),
        region.clone(),
        access_key_id.clone(),
        secret_access_key.clone(),
        &src_bucket,
    )
    .await;
    let dst_op = build_operator_for_bucket(
        Some(endpoint.clone()),
        region.clone(),
        access_key_id,
        secret_access_key,
        &dst_bucket,
    )
    .await;

    let keys = [
        format!("videos/{job_id}.m3u8"),
        format!("videos/{job_id}-001.m4s"),
        format!("videos/480/{job_id}.m3u8"),
        format!("videos/480/{job_id}-001.m4s"),
    ];

    let master_playlist = b"#EXTM3U\n#EXT-X-VERSION:3\n";
    let variant_playlist = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n";
    let segment_bytes = b"fake-segment";

    // Best-effort cleanup before seeding (in case previous runs left data behind).
    for key in &keys {
        let _ = src_op.delete(key).await;
        let _ = dst_op.delete(key).await;
    }

    src_op
        .write(&keys[0], master_playlist.to_vec())
        .await
        .unwrap();
    src_op
        .write(&keys[1], segment_bytes.to_vec())
        .await
        .unwrap();
    src_op
        .write(&keys[2], variant_playlist.to_vec())
        .await
        .unwrap();
    src_op
        .write(&keys[3], segment_bytes.to_vec())
        .await
        .unwrap();

    let token = server
        .create_claim(&client, &job_id, vec![], 3600)
        .await
        .unwrap();

    // Serve from src bucket via external (claim-protected) endpoint.
    let response = server
        .get_with_auth(
            &client,
            &format!("/videos/{src_bucket}/{job_id}.m3u8"),
            &token,
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), master_playlist);

    // Serve a variant playlist with an explicit bucket prefix.
    let response = server
        .get_with_auth(
            &client,
            &format!("/videos/{src_bucket}/480/{job_id}.m3u8"),
            &token,
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), variant_playlist);

    // Legacy read path without bucket should use s3_bucket default.
    let response = server
        .get_with_auth(&client, &format!("/videos/{job_id}.m3u8"), &token)
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), master_playlist);

    // Legacy read path with a width prefix should also use s3_bucket default.
    let response = server
        .get_with_auth(&client, &format!("/videos/480/{job_id}.m3u8"), &token)
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), variant_playlist);

    // Migrate only 480p + master prefixes.
    let request = MigrateRequest {
        job_id: job_id.clone(),
        src_bucket: src_bucket.clone(),
        dst_bucket: dst_bucket.clone(),
        widths: Some(vec![480]),
        dry_run: false,
    };
    let response = client
        .post(format!("{}/migrate", server.int_url()))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let migrate: MigrateResponse = response.json().await.unwrap();
    assert_eq!(migrate.job_id, job_id);
    assert_eq!(migrate.src_bucket, src_bucket);
    assert_eq!(migrate.dst_bucket, dst_bucket);
    assert!(!migrate.dry_run);
    assert_eq!(migrate.objects_total, 4);
    assert_eq!(migrate.objects_copied, 4);

    // Verify objects exist in dst bucket and can be served.
    assert_eq!(
        dst_op.read(&keys[0]).await.unwrap().to_bytes().as_ref(),
        master_playlist
    );
    assert_eq!(
        dst_op.read(&keys[2]).await.unwrap().to_bytes().as_ref(),
        variant_playlist
    );

    let response = server
        .get_with_auth(
            &client,
            &format!("/videos/{dst_bucket}/{job_id}.m3u8"),
            &token,
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), master_playlist);

    // Best-effort cleanup after the test.
    for key in &keys {
        let _ = src_op.delete(key).await;
        let _ = dst_op.delete(key).await;
    }
}
