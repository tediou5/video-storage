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

pub enum JobResult<N: Job, R: Job> {
    Done,
    Next(N),
    Retry(R),
    Err(FailureJob),
}

impl<N: Job, R: Job> From<Option<N>> for JobResult<N, R> {
    fn from(job: Option<N>) -> Self {
        match job {
            Some(job) => JobResult::Next(job),
            None => JobResult::Done,
        }
    }
}

impl<N: Job, R: Job> JobResult<N, R> {
    pub fn into_raw(self) -> JobResult<RawJob, RawJob> {
        match self {
            JobResult::Done => JobResult::Done,
            JobResult::Next(job) => JobResult::Next(RawJob::new(job)),
            JobResult::Retry(job) => JobResult::Retry(RawJob::new(job)),
            JobResult::Err(error) => JobResult::Err(error),
        }
    }
}

pub trait Job: Clone + Sized + Send + Sync + 'static {
    fn kind(&self) -> JobKind;

    fn need_permit(&self) -> bool {
        false
    }

    fn id(&self) -> &str;

    fn gen_job(&self, state: AppState) -> TokioJoinHandle<anyhow::Result<()>>;

    fn next_job(&self, _state: AppState) -> Option<impl Job> {
        Option::<Self>::None
    }

    fn wait_for_retry(&self) -> Option<Duration> {
        None
    }

    fn on_final_failure(&self) -> FailureJob;

    fn gen_task(
        &self,
        state: AppState,
        semaphore: Arc<TokioSemaphore>,
    ) -> impl Future<Output = JobResult<impl Job, impl Job>> + Send {
        async move {
            let job = self.clone();
            let job_id = job.id().to_string();
            let kind = job.kind();
            debug!(job_id, kind, "job wait for permit");

            let _permit = if self.need_permit() {
                Some(semaphore.acquire().await.unwrap())
            } else {
                None
            };

            info!(job_id, kind, "job started");

            let result = self.gen_job(state.clone()).await;
            let Err(error) = result.expect("Tokio task Join failed") else {
                // remove job from JobsManager if process successfully
                return JobResult::<_, Self>::from(self.next_job(state.clone()));
            };

            if let Some(retry_interval) = self.wait_for_retry() {
                error!(?error, %job_id, kind = self.kind(), "Job process failed, wait for retry");
                tokio::time::sleep(retry_interval).await;
                JobResult::Retry(job)
            } else {
                error!(?error, job_id, kind, "Job final failure");
                JobResult::Err(self.on_final_failure())
            }
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
    Cleanup,
    Webhook { message: String },
}

impl Action {
    async fn execute_action(action: Action, state: &AppState, job_id: &str, kind: JobKind) {
        match action {
            Action::Silent => {
                debug!(job_id, "Silent handling failed");
            }
            Action::Cleanup => {
                info!(job_id, kind, "Performing cleanup");
                match kind {
                    "convert-job" => {
                        let _ = tokio::fs::remove_file(state.uploads_dir().join(job_id)).await;
                    }
                    "upload-job" => {
                        let _ = tokio::fs::remove_dir_all(state.videos_dir().join(job_id)).await;
                    }
                    _ => {}
                }
            }
            Action::Webhook { message } => {
                info!(job_id, kind, message, "Calling webhook(unimplemented)");
                // TODO: call webhook
                // state.call_webhook(job_id, kind, "failed").await;
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

impl FailureJob {
    pub fn new(job_id: String, kind: JobKind, actions: Vec<Action>) -> Self {
        Self {
            job_id,
            kind,
            actions,
        }
    }

    pub async fn execute_actions(self, state: &AppState) {
        state.jobs_manager.remove(&self.job_id).await;
        for action in self.actions {
            Action::execute_action(action, state, &self.job_id, self.kind).await;
        }
    }
}
