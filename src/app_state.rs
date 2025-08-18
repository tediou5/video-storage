use crate::{Job, JobGenerator, StreamMap};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use tokio::sync::{Mutex as TokioMutex, Semaphore};
use tracing::{debug, error, info, warn};

const TEMP_DIR: &str = "temp";
const UPLOADS_DIR: &str = "uploads";
const VIDEOS_DIR: &str = "videos";
const JOB_FILE: &str = "pending-jobs.json";
pub(crate) const VIDEO_OBJECTS_DIR: &str = "video-objects";

async fn init_workspace(workspace: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(workspace.join(TEMP_DIR)).await?;
    tokio::fs::create_dir_all(workspace.join(UPLOADS_DIR)).await?;
    tokio::fs::create_dir_all(workspace.join(VIDEOS_DIR)).await?;
    Ok(())
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) token_rate: f64,
    pub(crate) job_set: Arc<TokioMutex<BTreeSet<Job>>>,
    pub(crate) job_tx: UnboundedSender<JobGenerator>,

    temp_dir: PathBuf,
    videos_dir: PathBuf,
    uploads_dir: PathBuf,
    pending_jobs_path: PathBuf,
}

impl AppState {
    pub(crate) async fn new(
        token_rate: f64,
        permits: usize,
        workspace: &Path,
    ) -> std::io::Result<Self> {
        init_workspace(workspace).await?;
        let (tx, rx) = unbounded();
        let this = Self {
            token_rate,
            job_set: Arc::default(),
            job_tx: tx,

            temp_dir: workspace.join(TEMP_DIR),
            uploads_dir: workspace.join(UPLOADS_DIR),
            videos_dir: workspace.join(VIDEOS_DIR),
            pending_jobs_path: workspace.join(JOB_FILE),
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

    fn handle_jobs(&self, rx: UnboundedReceiver<JobGenerator>, permits: usize) {
        info!(permits, "Job handler started");
        let this = self.clone();
        let semaphore = Arc::new(Semaphore::new(permits));

        tokio::spawn(async move {
            debug!("Job handler started #1");
            let rx = std::pin::pin!(rx);
            let mut rx = rx.fuse();

            let mut jobs: StreamMap<'_, String, Result<(), JobGenerator>> = StreamMap::default();
            loop {
                debug!("Waiting for job");
                futures::select! {
                    maybe_job = rx.next() => {
                        debug!(is_some = maybe_job.is_some() ,"Job received");
                        let Some(generator) = maybe_job else {
                            error!("Job handler finished");
                            break;
                        };

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
                                generator
                            });

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
                    }
                    (id, result) = jobs.select_next_some() => {
                        let Err(job) = result else {
                            info!("Job {id} completed successfully");
                            // update job status
                            let mut js = this.job_set.lock().await;
                            js.retain(|job| job.id() != id);
                            let dump = serde_json::to_string(&*js).unwrap();
                            // update jobs file
                            _ = tokio::fs::write(this.pending_jobs_path(), dump).await;
                            continue;
                        };

                        warn!(%id, "Retry job");
                        _ = this.job_tx.unbounded_send(job);
                    }
                }
            }

            debug!("Job handler finished");
        });
    }
}
