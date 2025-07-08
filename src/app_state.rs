use crate::{Job, JobGenerator, StreamMap};

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use tokio::sync::{Mutex as TokioMutex, Semaphore};
use tracing::{debug, error, info, warn};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) token_rate: f64,
    pub(crate) job_set: Arc<TokioMutex<BTreeSet<Job>>>,
    pub(crate) job_tx: UnboundedSender<JobGenerator>,
}

impl AppState {
    pub(crate) fn new(token_rate: f64, permits: usize) -> Self {
        let (tx, rx) = unbounded();
        let this = Self {
            token_rate,
            job_set: Arc::default(),
            job_tx: tx,
        };

        this.handle_jobs(rx, permits);
        this
    }

    fn handle_jobs(&self, rx: UnboundedReceiver<JobGenerator>, permits: usize) {
        info!(permits, "Job handler started");
        let tx = self.job_tx.clone();
        let job_set = self.job_set.clone();
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

                        let id = generator.id().to_string();
                        let semaphore_c = semaphore.clone();
                        let task = async move {
                            debug!(id = %generator.id(), "Job wait for permit");

                            let _permit = semaphore_c.acquire().await.unwrap();
                            info!(id = %generator.id(), "Job started");

                            let result = generator.gen_task().await;
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
                    }
                    (id, result) = jobs.select_next_some() => {
                        let Err(job) = result else {
                            info!("Job {id} completed successfully");
                            // clean up upload file & update job status
                            let mut js = job_set.lock().await;
                            js.retain(|job| job.id() != id);
                            let dump = serde_json::to_string(&*js).unwrap();
                            // update jobs file
                            _ = tokio::fs::write(crate::JOB_FILE, dump).await;
                            // remove upload file
                            let upload_dir = PathBuf::from(crate::UPLOADS_DIR);
                            let upload_path = upload_dir.join(&id);
                            if let Err(error) = tokio::fs::remove_file(&upload_path).await {
                                error!(%error, %id, ?upload_path, "Failed to remove upload file");
                            } else {
                                info!(%id, ?upload_path, "Upload file removed");
                            };
                            let temp_vp9_dir = PathBuf::from(crate::TEMP_DIR).join(format!("tmp-vp9-{id}"));
                            // let vp9_file = temp_vp9_dir.join("vp9.webm");
                            if let Err(error) = tokio::fs::remove_dir_all(&temp_vp9_dir).await {
                                error!(%error, %id, ?temp_vp9_dir, "Failed to remove tmp vp9 dir");
                            } else {
                                info!(%id, ?temp_vp9_dir, "Tmp vp9 dir removed");
                            };

                            continue;
                        };

                        warn!(%id, "Retry job");
                        _ = tx.unbounded_send(job);
                    }
                }
            }

            debug!("Job handler finished");
        });
    }
}
