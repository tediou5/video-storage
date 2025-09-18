pub mod convert;
pub mod manager;
pub mod raw;
pub mod upload;

use crate::app_state::AppState;
use std::{ops::Deref, sync::Arc, time::Duration};
use tokio::sync::Semaphore as TokioSemaphore;
use tokio::task::JoinHandle as TokioJoinHandle;
use tracing::{debug, error, info};

// Re-exports for convenience
pub use convert::ConvertJob;
pub use raw::RawJob;
pub use upload::UploadJob;

pub type JobKind = &'static str;
pub const UPLOAD_KIND: JobKind = "upload";
pub const CONVERT_KIND: JobKind = "convert";

pub type JobResult<J: Job> = Result<Option<J>, FailureJob>;

pub trait Job: Clone + Sized + Send + Sync + 'static {
    fn kind(&self) -> JobKind;
    fn need_permit(&self) -> bool {
        true
    }

    fn id(&self) -> &str;

    fn gen_job(&self, state: AppState) -> TokioJoinHandle<anyhow::Result<()>>;

    fn next_job(&self, state: AppState) -> Option<impl Job>;

    // wait for retry interval, return true if retry is needed
    fn wait_for_retry(&self) -> Option<Duration>;

    fn on_final_failure(&self) -> FailureJob;

    fn gen_task(
        &self,
        state: AppState,
        semaphore: Arc<TokioSemaphore>,
    ) -> impl Future<Output = Result<JobResult<impl Job>, Self>> + Send {
        async move {
            let job = self.clone();
            let job_id = job.id().to_string();
            debug!(job_id, kind = self.kind(), "job wait for permit");

            let _permit = if self.need_permit() {
                Some(semaphore.acquire().await.unwrap())
            } else {
                None
            };
            info!(job_id, kind = self.kind(), "job started");

            let result = self.gen_job(state.clone()).await;
            let mut result = result
                .expect("Tokio task Join failed")
                .map_err(|error| {
                    error!(?error, %job_id, kind = self.kind(), "Job process failed, wait for retry");
                    job
                })
                .map(|_| Ok(self.next_job(state.clone())));

            if result.is_err() {
                if let Some(retry_interval) = self.wait_for_retry() {
                    tokio::time::sleep(retry_interval).await;
                } else {
                    result = result.map(|_| {
                        error!(%job_id, kind = self.kind(), "Job final failure, no longer retry again");
                        Err(self.on_final_failure())
                    });
                }
            } else {
                // remove job from JobsManager if process successfully
                state.jobs_manager.remove(&job_id).await;
            }

            result
        }
    }
}

impl<T, J> Job for T
where
    T: Deref<Target = J> + Clone + Send + Sync + 'static,
    J: Job,
{
    fn kind(&self) -> JobKind {
        self.deref().kind()
    }

    fn id(&self) -> &str {
        self.deref().id()
    }

    fn gen_job(&self, state: AppState) -> TokioJoinHandle<anyhow::Result<()>> {
        self.deref().gen_job(state)
    }

    fn next_job(&self, state: AppState) -> Option<impl Job> {
        self.deref().next_job(state)
    }

    fn wait_for_retry(&self) -> Option<Duration> {
        self.deref().wait_for_retry()
    }

    fn on_final_failure(&self) -> FailureJob {
        self.deref().on_final_failure()
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Silent,
    Webhook { kind: JobKind, message: String },
    Cleanup { kind: JobKind, job_id: String },
}

impl Action {
    async fn execute_action(action: Action, state: &AppState, job_id: &str) {
        // remove job from JobsManager
        state.jobs_manager.remove(job_id).await;
        match action {
            Action::Silent => {
                debug!(job_id, "Silent handling failed");
            }
            Action::Webhook { kind, message } => {
                info!(job_id, kind, message, "Calling webhook(unimplemented)");
                // TODO: call webhook
                // state.call_webhook(job_id, kind, "failed").await;
            }
            Action::Cleanup { kind, job_id } => {
                info!(job_id, kind, "Performing cleanup");
                match kind {
                    "convert-job" => {
                        let _ = tokio::fs::remove_file(state.uploads_dir().join(&job_id)).await;
                    }
                    "upload-job" => {
                        let _ = tokio::fs::remove_dir_all(state.videos_dir().join(&job_id)).await;
                    }
                    _ => {}
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct FailureJob {
    pub job_id: String,
    pub kind: JobKind,
    pub actions: Vec<Action>,
}

impl Job for FailureJob {
    fn kind(&self) -> JobKind {
        "failure-job"
    }

    fn need_permit(&self) -> bool {
        false
    }

    fn id(&self) -> &str {
        &self.job_id
    }

    fn gen_job(&self, state: AppState) -> TokioJoinHandle<anyhow::Result<()>> {
        let actions = self.actions.clone();
        let job_id = self.job_id.clone();

        tokio::spawn(async move {
            for action in actions {
                Action::execute_action(action, &state, &job_id).await;
            }
            Ok(())
        })
    }

    fn next_job(&self, _state: AppState) -> Option<impl Job> {
        Option::<Self>::None
    }

    fn wait_for_retry(&self) -> Option<Duration> {
        None // NoJob never retries
    }

    fn on_final_failure(&self) -> FailureJob {
        panic!("FailureJob never failed")
    }
}

impl FailureJob {
    pub fn new(job_id: String, kind: JobKind, actions: Vec<Action>) -> Self {
        Self {
            job_id,
            kind,
            actions,
        }
    }
}
