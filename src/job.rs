use crate::app_state::AppState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::sync::Semaphore as TokioSemaphore;
use tokio::task::JoinHandle as TokioJoinHandle;
use tracing::{debug, error, info};

type JobKind = &'static str;
trait Job: Clone + Sized {
    const KIND: JobKind;
    type NextJob: Job + 'static;

    fn id(&self) -> &str;

    fn gen_job(&self, state: AppState) -> TokioJoinHandle<anyhow::Result<()>>;

    fn next_job(&self) -> Option<Self::NextJob>;

    fn gen_task(
        self,
        state: AppState,
        semaphore: TokioSemaphore,
        retry_interval_secs: u64,
    ) -> impl Future<Output = Result<Option<impl Job>, Self>> {
        use tokio::time::{Duration, sleep};

        async move {
            let job = self.clone();
            let job_id = job.id().to_string();
            debug!(job_id, kind = Self::KIND, "job wait for permit");

            let _permit = semaphore.acquire().await.unwrap();
            info!(job_id, kind = Self::KIND, "job started");

            let result = self.gen_job(state).await;
            let result = result
                .expect("Tokio task Join failed")
                .map_err(|error| {
                    error!(%error, job_id, kind = Self::KIND, "Job process failed, retry later");
                    self
                })
                .map(|_| job.next_job());

            if result.is_err() {
                sleep(Duration::from_secs(retry_interval_secs)).await;
            }

            result
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct UploadJob {
    pub id: String,
}

impl UploadJob {
    #[allow(unused)]
    pub fn new(id: String) -> Self {
        Self { id }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path(&self, video_dir: &Path) -> PathBuf {
        video_dir.join(&self.id)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConvertJob {
    pub id: String,
    pub crf: u8, // 0-63
}

impl ConvertJob {
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Clone, Debug)]
pub struct JobGenerator {
    pub job: ConvertJob,
    pub upload_path: PathBuf,
}

impl JobGenerator {
    pub fn new(job: ConvertJob, upload_path: PathBuf) -> Self {
        Self { job, upload_path }
    }

    pub fn id(&self) -> &str {
        self.job.id()
    }

    pub fn gen_task(&self, state: AppState) -> TokioJoinHandle<anyhow::Result<()>> {
        let this = self.clone();
        tokio::spawn(async move {
            let job_id = this.job.id().to_string();
            crate::spawn_hls_job(this, state.videos_dir(), state.temp_dir())
                .await
                .inspect_err(|error| error!(%job_id, %error, "Failed to process video"))
        })
    }
}
