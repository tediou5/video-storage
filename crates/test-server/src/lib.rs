use ffmpeg_next as ffmpeg;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::OnceCell;
use tokio::time::sleep;
pub use video_storage_claim::{CreateClaimRequest, CreateClaimResponse};
use video_storage_core::Config;
use video_storage_core::job::convert::{ConvertJob, Scales};

pub static SHARED_SERVER: OnceCell<TestServer> = OnceCell::const_new();
pub static SHARED_WEBHOOK: OnceCell<MockWebhook> = OnceCell::const_new();

#[derive(serde::Deserialize)]
pub struct UploadResponse {
    pub job_id: String,
    #[allow(dead_code)]
    pub message: String,
}

/// Test harness that manages the server process
pub struct TestServer {
    _handle: JoinHandle<()>,
    e_port: u16,
    i_port: u16,
}

impl TestServer {
    /// Get or create shared test server instance
    pub async fn shared() -> &'static TestServer {
        SHARED_SERVER
            .get_or_init(|| async { Self::start_with_webhook("").await })
            .await
    }

    /// Get or create shared test server with webhook
    pub async fn shared_with_webhook() -> &'static TestServer {
        SHARED_SERVER
            .get_or_init(|| async {
                // wait for auth tests server to start
                tokio::time::sleep(Duration::from_millis(100)).await;
                let webhook = SHARED_WEBHOOK
                    .get_or_init(|| async { MockWebhook::start().await })
                    .await;
                Self::start_with_webhook(&webhook.url()).await
            })
            .await
    }

    /// Start the server with webhook URL (private)
    async fn start_with_webhook(webhook_url: &str) -> Self {
        // Only open when debugging
        tracing_subscriber::fmt::init();

        // Initialize ffmpeg once
        static FFMPEG_INIT: std::sync::Once = std::sync::Once::new();
        FFMPEG_INIT.call_once(|| {
            ffmpeg::init().unwrap();
        });

        // Find an available port
        let e_port = portpicker::pick_unused_port().expect("No available port");
        let i_port = portpicker::pick_unused_port().expect("No available port");

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();

        let workspace = format!("/tmp/test-workspace-{now}");

        // Clean up existing workspace
        let _ = tokio::fs::remove_dir_all(&workspace).await;

        let config = Config {
            listen_on_port: e_port,
            internal_port: i_port,
            workspace: workspace.clone(),
            webhook_url: if webhook_url.is_empty() {
                None
            } else {
                Some(webhook_url.to_string())
            },
            ..Default::default()
        };

        // Spawn the server in a separate thread with its own runtime
        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                video_storage_core::run(config).await;
            });
        });

        let server = TestServer {
            _handle: handle,
            e_port,
            i_port,
        };

        // Wait for server to be ready
        let client = server.client();

        sleep(Duration::from_millis(1)).await;
        // Poll until server is ready
        for _ in 0..200 {
            if let Ok(response) = client
                .get(format!("http://127.0.0.1:{}/waitlist", server.i_port))
                .send()
                .await
                && response.status().is_success()
            {
                break;
            }

            sleep(Duration::from_millis(10)).await;
        }

        server
    }

    pub fn int_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.i_port)
    }

    pub fn ext_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.e_port)
    }

    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30)) // Longer timeout for upload/convert operations
            .build()
            .unwrap()
    }

    /// Create a claim token via HTTP API
    pub async fn create_claim(
        &self,
        client: &reqwest::Client,
        asset_id: &str,
        allowed_widths: Vec<u16>,
        exp_offset: i64,
    ) -> Result<String, Box<dyn std::error::Error>> {
        self.create_claim_with_concurrency(
            client,
            asset_id,
            Some(allowed_widths),
            exp_offset,
            Some(3),
        )
        .await
    }

    /// Create a claim token via HTTP API with concurrency control
    pub async fn create_claim_with_concurrency(
        &self,
        client: &reqwest::Client,
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
            asset_id: asset_id.into(),
            nbf_unix: Some(now - 1), // Valid from 1 second ago
            exp_unix: (now as i64 + exp_offset) as u32,
            window_len_sec: Some(300),
            max_kbps: Some(5000),
            max_concurrency,
            allowed_widths,
        };

        let url = format!("{}/claims", self.int_url());
        let response = client.post(&url).json(&request).send().await?;

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

    /// Create a v2 claim token with multiple asset IDs
    pub async fn create_claim_v2(
        &self,
        client: &reqwest::Client,
        asset_ids: Vec<String>,
        allowed_widths: Vec<u16>,
        exp_offset: i64,
    ) -> Result<String, Box<dyn std::error::Error>> {
        self.create_claim_v2_with_concurrency(
            client,
            asset_ids,
            Some(allowed_widths),
            exp_offset,
            Some(3),
        )
        .await
    }

    /// Create a v2 claim token with multiple asset IDs and concurrency control
    pub async fn create_claim_v2_with_concurrency(
        &self,
        client: &reqwest::Client,
        asset_ids: Vec<String>,
        allowed_widths: Option<Vec<u16>>,
        exp_offset: i64,
        max_concurrency: Option<u16>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;

        let request = CreateClaimRequest {
            asset_id: asset_ids.into(), // This creates AssetsFilter::List
            nbf_unix: Some(now - 1),    // Valid from 1 second ago
            exp_unix: (now as i64 + exp_offset) as u32,
            window_len_sec: Some(300),
            max_kbps: Some(5000),
            max_concurrency,
            allowed_widths,
        };

        let url = format!("{}/claims", self.int_url());
        let response = client.post(&url).json(&request).send().await?;

        if !response.status().is_success() {
            return Err(format!(
                "Failed to create v2 claim: {}, url: {url}, request: {request:?}",
                response.status(),
            )
            .into());
        }

        let claim_response: CreateClaimResponse = response.json().await?;
        Ok(claim_response.token)
    }

    /// Make an authenticated GET request
    pub async fn get_with_auth(
        &self,
        client: &reqwest::Client,
        path: &str,
        token: &str,
    ) -> reqwest::Response {
        client
            .get(format!("{}{}", self.ext_url(), path))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .unwrap()
    }

    /// Make an unauthenticated GET request
    pub async fn get_without_auth(
        &self,
        client: &reqwest::Client,
        path: &str,
    ) -> reqwest::Response {
        client
            .get(format!("{}{}", self.int_url(), path))
            .send()
            .await
            .unwrap()
    }

    /// Upload a video file with 480p-only scales using serde_qs
    pub async fn upload_video_with_scales(
        &self,
        client: &reqwest::Client,
        job_id: &str,
        crf: u8,
        scales: Scales,
        video_data: Vec<u8>,
    ) -> Result<UploadResponse, Box<dyn std::error::Error>> {
        // Create ConvertJob with 480p scale only
        let job = ConvertJob::new(job_id.to_string(), crf, scales);

        // Use serde_qs to encode the query parameters
        let query_string = serde_qs::to_string(&job)?;
        let url = format!("{}/upload?{}", self.int_url(), query_string);

        let response = client
            .post(&url)
            .header("Content-Type", "application/octet-stream")
            .body(video_data)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await?;
            return Err(format!("Upload failed with status {}: {}", status, error_text).into());
        }

        let upload_response: UploadResponse = response.json().await?;
        Ok(upload_response)
    }

    /// Get shared webhook instance
    pub fn shared_webhook(&self) -> &'static MockWebhook {
        SHARED_WEBHOOK.get().expect("Webhook not initialized")
    }
}

/// Mock webhook server for testing webhook callbacks
pub struct MockWebhook {
    server_handle: tokio::task::JoinHandle<()>,
    pub port: u16,
    pub received_calls: std::sync::Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>,
}

impl MockWebhook {
    pub async fn start() -> Self {
        let port = portpicker::pick_unused_port().expect("No available port for webhook");
        let received_calls = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let received_calls_clone = received_calls.clone();

        let server_handle = tokio::spawn(async move {
            use warp::Filter;

            let webhook = warp::path("webhook")
                .and(warp::post())
                .and(warp::body::json())
                .map(move |body: serde_json::Value| {
                    let received_calls = received_calls_clone.clone();
                    tokio::spawn(async move {
                        received_calls.lock().await.push(body);
                    });
                    warp::reply::with_status("OK", warp::http::StatusCode::OK)
                });

            warp::serve(webhook).run(([127, 0, 0, 1], port)).await;
        });

        // Wait a bit for server to start
        sleep(Duration::from_millis(100)).await;

        MockWebhook {
            server_handle,
            port,
            received_calls,
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/webhook", self.port)
    }

    pub async fn get_calls(&self) -> Vec<serde_json::Value> {
        self.received_calls.lock().await.clone()
    }

    pub async fn wait_for_calls(&self, expected_count: usize, timeout_secs: u64) -> bool {
        let start = std::time::Instant::now();

        while start.elapsed() < Duration::from_secs(timeout_secs) {
            if self.received_calls.lock().await.len() >= expected_count {
                return true;
            }
            sleep(Duration::from_millis(100)).await;
        }
        false
    }
}

impl Drop for MockWebhook {
    fn drop(&mut self) {
        self.server_handle.abort();
    }
}
