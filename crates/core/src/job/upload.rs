use crate::app_state::AppState;
use crate::job::{Action, FailureJob, Job, JobKind, UPLOAD_KIND};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::task::JoinHandle as TokioJoinHandle;
use tracing::error;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct UploadJob {
    pub id: String,
}

impl UploadJob {
    pub fn new(id: String) -> Self {
        Self { id }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get the video directory path for this job
    pub fn video_path(&self, videos_dir: &Path) -> PathBuf {
        videos_dir.join(&self.id)
    }
}

impl Job for UploadJob {
    fn kind(&self) -> JobKind {
        UPLOAD_KIND
    }

    fn need_permit(&self) -> usize {
        1
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn gen_job(&self, state: AppState) -> TokioJoinHandle<anyhow::Result<()>> {
        let upload_path = self.video_path(state.videos_dir());
        let job_id = self.id.clone();

        tokio::spawn(async move {
            state
                .storage_manager
                .upload_directory(&upload_path, "videos")
                .await
                .map(|_| {
                    _ = std::fs::remove_dir_all(upload_path);
                })
                .inspect_err(|error| error!(%job_id, %error, "Failed to upload video directory"))
        })
    }

    fn wait_for_retry(&self) -> Option<Duration> {
        Some(Duration::from_secs(5))
    }

    fn on_final_failure(&self) -> FailureJob {
        FailureJob::new(
            self.id.clone(),
            self.kind(),
            vec![
                Action::Cleanup,
                Action::Webhook {
                    message: "Upload job failed after all retries".to_string(),
                },
            ],
        )
    }
}
