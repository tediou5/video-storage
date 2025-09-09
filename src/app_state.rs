use crate::opendal::StorageManager;
use crate::{ConvertJob, JobGenerator, StreamMap, UploadJob};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex as TokioMutex, Semaphore};
use tracing::{debug, error, info, warn};

const TEMP_DIR: &str = "temp";
const UPLOADS_DIR: &str = "uploads";
const VIDEOS_DIR: &str = "videos";
const JOB_FILE: &str = "pending-jobs.json";
const UPLOAD_JOB_FILE: &str = "pending-uploads.json";
pub(crate) const VIDEO_OBJECTS_DIR: &str = "video-objects";

async fn init_workspace(workspace: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(workspace.join(TEMP_DIR)).await?;
    tokio::fs::create_dir_all(workspace.join(UPLOADS_DIR)).await?;
    tokio::fs::create_dir_all(workspace.join(VIDEOS_DIR)).await?;
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) enum Job {
    Convert(JobGenerator),
    Upload(UploadJob),
}

type JobKind = &'static str;
impl Job {
    const KIND_CONVERT: JobKind = "convert-job";
    const KIND_UPLOAD: JobKind = "upload-job";

    fn id(&self) -> &str {
        match self {
            Job::Convert(job) => job.id(),
            Job::Upload(job) => job.id(),
        }
    }

    fn kind(&self) -> JobKind {
        match self {
            Job::Convert(_) => Self::KIND_CONVERT,
            Job::Upload(_) => Self::KIND_UPLOAD,
        }
    }
}

impl From<JobGenerator> for Job {
    fn from(job: JobGenerator) -> Self {
        Job::Convert(job)
    }
}

impl From<UploadJob> for Job {
    fn from(job: UploadJob) -> Self {
        Job::Upload(job)
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) token_rate: f64,
    pub(crate) job_set: Arc<TokioMutex<BTreeSet<ConvertJob>>>,
    pub(crate) upload_job_set: Arc<TokioMutex<BTreeSet<UploadJob>>>,
    pub(crate) job_tx: UnboundedSender<Job>,
    pub(crate) storage_manager: Arc<StorageManager>,
    pub(crate) webhook_url: Option<String>,

    temp_dir: PathBuf,
    videos_dir: PathBuf,
    uploads_dir: PathBuf,
    pending_jobs_path: PathBuf,
    pending_uploads_path: PathBuf,
}

impl AppState {
    pub(crate) async fn new(
        token_rate: f64,
        permits: usize,
        workspace: &Path,
        storage_manager: StorageManager,
        webhook_url: Option<String>,
    ) -> std::io::Result<Self> {
        init_workspace(workspace).await?;
        let (tx, rx) = unbounded();
        let this = Self {
            token_rate,
            job_set: Arc::default(),
            upload_job_set: Arc::default(),
            job_tx: tx,
            storage_manager: Arc::new(storage_manager),
            webhook_url,

            temp_dir: workspace.join(TEMP_DIR),
            uploads_dir: workspace.join(UPLOADS_DIR),
            videos_dir: workspace.join(VIDEOS_DIR),
            pending_jobs_path: workspace.join(JOB_FILE),
            pending_uploads_path: workspace.join(UPLOAD_JOB_FILE),
        };

        this.handle_jobs(rx, permits);
        Ok(this)
    }

    pub(crate) fn temp_dir(&self) -> &Path {
        self.temp_dir.as_path()
    }

    pub(crate) fn uploads_dir(&self) -> &Path {
        self.uploads_dir.as_path()
    }

    pub(crate) fn videos_dir(&self) -> &Path {
        self.videos_dir.as_path()
    }

    pub(crate) fn pending_jobs_path(&self) -> &Path {
        self.pending_jobs_path.as_path()
    }

    pub(crate) fn pending_uploads_path(&self) -> &Path {
        self.pending_uploads_path.as_path()
    }

    async fn call_webhook(&self, job_id: &str, job_type: &str, status: &str) {
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
                Err(err) => {
                    error!(job_id, webhook_url, ?err, "Failed to call webhook");
                }
            }
        }
    }

    fn handle_jobs(&self, rx: UnboundedReceiver<Job>, permits: usize) {
        info!(permits, "Job handler started");
        let this = self.clone();
        let semaphore = Arc::new(Semaphore::new(permits));

        tokio::spawn(async move {
            debug!("Job handler started #1");
            let rx = std::pin::pin!(rx);
            let mut rx = rx.fuse();

            let mut jobs: StreamMap<'_, String, Result<JobKind, Job>> = StreamMap::default();
            loop {
                debug!("Waiting for job");
                futures::select! {
                    maybe_job = rx.next() => {
                        debug!(is_some = maybe_job.is_some() ,"Job received");
                        let Some(job) = maybe_job else {
                            error!("Job handler finished");
                            break;
                        };

                        match job {
                            Job::Convert(generator) => {
                                let job = generator.job.clone();
                                let id = generator.id().to_string();
                                let this_c = this.clone();
                                let semaphore_c = semaphore.clone();
                                let task = async move {
                                    debug!(id = %generator.id(), "Job wait for permit");

                                    let _permit = semaphore_c.acquire().await.unwrap();
                                    info!(id = %generator.id(), "Job started");

                                    let result = generator.gen_task(this_c).await;
                                    let result = result.expect("Tokio task Join failed").map_err(|error| {
                                        error!(%error, id = %generator.id(), "Job process failed, retry later");
                                        Job::Convert(generator)
                                    }).map(|_| Job::KIND_CONVERT);

                                    if result.is_err() {
                                        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                                    }

                                    result
                                };
                                if !jobs.add_if_not_in_progress(id.clone(), Box::pin(task)) {
                                    warn!("Job {id} already in-progress, skipping");
                                    continue;
                                }
                                info!(id, "Job added to job set");
                                this.job_set.lock().await.insert(job);
                                let dump = serde_json::to_string(&*this.job_set.lock().await).unwrap();
                                // update jobs file
                                _ = tokio::fs::write(this.pending_jobs_path(), dump).await;
                            },
                            Job::Upload(upload_job) => {
                                let id = upload_job.id().to_string();
                                let this_c = this.clone();
                                let semaphore_c = semaphore.clone();
                                let upload_job_clone = upload_job.clone();

                                let task = async move {
                                    debug!(id = %upload_job.id(), "Upload job wait for permit");

                                    let _permit = semaphore_c.acquire().await.unwrap();
                                    info!(id = %upload_job.id(), "Upload job started");

                                    let result = this_c.storage_manager
                                        .upload_directory(upload_job.path.clone(), "videos")
                                        .await
                                        .map_err(|error| {
                                            error!(%error, id = %upload_job.id(), "Upload failed, retry later");
                                            Job::Upload(upload_job)
                                        })
                                        .map(|_| Job::KIND_UPLOAD);

                                    if result.is_err() {
                                        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                                    }

                                    result
                                };

                                if !jobs.add_if_not_in_progress(id.clone(), Box::pin(task)) {
                                    warn!("Upload job {id} already in-progress, skipping");
                                    continue;
                                }
                                info!(id, "Upload job added to job set");
                                this.upload_job_set.lock().await.insert(upload_job_clone);
                                let dump = serde_json::to_string(&*this.upload_job_set.lock().await).unwrap();
                                // update uploads file
                                _ = tokio::fs::write(this.pending_uploads_path(), dump).await;
                            },
                        }
                    }
                    (id, result) = jobs.select_next_some() => {
                        match result {
                            Ok(job_kind) if job_kind == Job::KIND_CONVERT => {
                                    info!("Convert Job {id} completed successfully");
                                    // update job status
                                    let mut js = this.job_set.lock().await;
                                    js.retain(|job| job.id() != id);
                                    let dump = serde_json::to_string(&*js).unwrap();
                                    // update jobs file
                                    _ = tokio::fs::write(this.pending_jobs_path(), dump).await;

                                    // Create upload job if using remote storage
                                    if this.storage_manager.is_remote() {
                                        let videos_path = this.videos_dir().join(&id);
                                        let upload_job = UploadJob {
                                            id: id.clone(),
                                            path: videos_path,
                                        };
                                        info!(id, "Creating upload job for remote storage");
                                        _ = this.job_tx.unbounded_send(upload_job.into());
                                    }

                                    this.call_webhook(&id, "convert", "completed").await;
                            },
                            Ok(job_kind) if job_kind == Job::KIND_UPLOAD => {
                                info!("Upload Job {id} completed successfully");
                                // update upload job status
                                let mut ujs = this.upload_job_set.lock().await;
                                ujs.retain(|job| job.id() != id);
                                let dump = serde_json::to_string(&*ujs).unwrap();
                                // update uploads file
                                _ = tokio::fs::write(this.pending_uploads_path(), dump).await;
                                // Call webhook after successful upload
                                this.call_webhook(&id, "upload", "completed").await;
                                _ = tokio::fs::remove_file(this.videos_dir.join(id)).await;
                            },
                            Err(job) => {
                                warn!(job_id=%job.id(), job_kind=%job.kind(), "Retry job");
                                _ = this.job_tx.unbounded_send(job);
                            },
                            _ => {}
                        }
                    }
                }
            }

            debug!("Job handler finished");
        });
    }
}
