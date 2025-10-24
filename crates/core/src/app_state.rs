use crate::job::manager::JobSetManager;
use crate::job::raw::RawJob;
use crate::job::{Job, JobResult};
use crate::{StorageManager, StreamMap};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::json;
use std::io::ErrorKind as StdIoErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};
use video_storage_claim::ClaimManager;

const TEMP_DIR: &str = "temp";
const UPLOADS_DIR: &str = "uploads";
const VIDEOS_DIR: &str = "videos";

fn init_workspace(workspace: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(workspace.join(TEMP_DIR))?;
    std::fs::create_dir_all(workspace.join(UPLOADS_DIR))?;
    std::fs::create_dir_all(workspace.join(VIDEOS_DIR))?;
    Ok(())
}

#[derive(Clone)]
pub struct AppState {
    pub job_tx: UnboundedSender<RawJob>,
    pub jobs_manager: JobSetManager,
    pub claim_manager: ClaimManager,
    pub storage_manager: Arc<StorageManager>,
    pub permits: usize,

    pub temp_dir: PathBuf,
    pub videos_dir: PathBuf,
    pub uploads_dir: PathBuf,
    pub webhook_url: Option<String>,
}

impl AppState {
    pub async fn new(
        permits: usize,
        workspace: &Path,
        storage_manager: StorageManager,
        webhook_url: Option<String>,
        claim_keys: Vec<(u8, [u8; 32])>,
    ) -> anyhow::Result<Self> {
        init_workspace(workspace)?;
        let (tx, rx) = unbounded();

        let jobs_manager = JobSetManager::new(workspace, &tx)?;

        // Initialize claim manager
        let claim_manager = ClaimManager::from_keys(claim_keys)
            .map_err(|error| std::io::Error::new(StdIoErrorKind::InvalidInput, error))?;

        let this = Self {
            job_tx: tx,
            jobs_manager,
            claim_manager,
            storage_manager: Arc::new(storage_manager),
            permits,

            temp_dir: workspace.join(TEMP_DIR),
            uploads_dir: workspace.join(UPLOADS_DIR),
            videos_dir: workspace.join(VIDEOS_DIR),
            webhook_url,
        };

        // Start the cleanup task after we're in async context
        this.claim_manager.start_cleanup_task();

        // Start job handler
        this.start_job_handler(rx, permits);

        Ok(this)
    }

    pub fn temp_dir(&self) -> &Path {
        self.temp_dir.as_path()
    }

    pub fn uploads_dir(&self) -> &Path {
        self.uploads_dir.as_path()
    }

    pub fn videos_dir(&self) -> &Path {
        self.videos_dir.as_path()
    }

    pub async fn call_webhook(&self, job_id: &str, job_type: &str, status: &str) {
        if let Some(webhook_url) = &self.webhook_url {
            let payload = json!({
                "job_id": job_id,
                "job_type": job_type,
                "status": status,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            });

            let client = reqwest::Client::new();
            match client
                .post(webhook_url)
                .json(&payload)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
            {
                Ok(response) => {
                    if response.status().is_success() {
                        info!(job_id, webhook_url, "Webhook called successfully");
                    } else {
                        warn!(
                            job_id,
                            webhook_url,
                            status = %response.status(),
                            "Webhook returned non-success status"
                        );
                    }
                }
                Err(error) => {
                    error!(job_id, webhook_url, ?error, "Failed to call webhook");
                }
            }
        }
    }

    fn start_job_handler(&self, rx: UnboundedReceiver<RawJob>, permits: usize) {
        info!(permits, "Job handler started");
        let this = self.clone();
        let semaphore = Arc::new(Semaphore::new(permits));

        tokio::spawn(async move {
            debug!("Job handler started with RawJob system");
            let rx = std::pin::pin!(rx);
            let mut rx = rx.fuse();

            let mut jobs: StreamMap<'_, String, JobResult<RawJob, RawJob>> = StreamMap::default();

            loop {
                debug!("Waiting for job");
                futures::select! {
                    maybe_job = rx.next() => {
                        debug!(is_some = maybe_job.is_some(), "Job received");
                        let Some(job) = maybe_job else {
                            error!("Job handler finished");
                            break;
                        };

                        let kind = job.kind();
                        let job_id = job.id().to_string();

                        let this_c = this.clone();
                        let semaphore_c = semaphore.clone();
                        let task = async move {
                            job.gen_task(this_c, semaphore_c).await.into_raw()
                        };
                        if !jobs.add_if_not_in_progress(job_id.clone(), Box::pin(task)) {
                            warn!("Job {job_id} already in-progress, skipping");
                            continue;
                        }

                        info!(job_id, kind, "Job added to processing queue");
                    }
                    (id, result) = jobs.select_next_some() => {
                        match result {
                            JobResult::Done => {
                                info!(id, "Job completed successfully");
                                this.jobs_manager.remove(&id).await;
                                continue;
                            },
                            JobResult::Next(next_job) => {
                                let kind = next_job.kind();
                                info!(id, kind, "NextJob added to processing queue");
                                _ = this.job_tx.unbounded_send(next_job);
                            },
                            JobResult::Retry(job) => {
                                warn!(job_id = %job.id(), "Retrying job");
                                _ = this.job_tx.unbounded_send(job);
                            },
                            JobResult::Err(failure_job) => {
                                info!(id, "Job failed with failure handling");
                                failure_job.execute_actions(&this).await;
                            },
                        }
                    }
                }
            }

            debug!("Job handler finished");
        });
    }
}
