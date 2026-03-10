use crate::app_state::AppState;
use crate::job::{Action, FailureJob, Job, JobKind, UPLOAD_KIND};
use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::task::JoinHandle as TokioJoinHandle;
use tracing::error;

fn is_pure_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct UploadJob {
    pub id: String,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_bucket: Option<String>,
}

impl UploadJob {
    pub fn new(id: String, dst_bucket: Option<String>) -> Self {
        Self { id, dst_bucket }
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
        let dst_bucket = self.dst_bucket.clone();

        tokio::spawn(async move {
            if !state.storage_manager.is_s3() {
                return Ok(());
            }

            let Some(dst_bucket) = dst_bucket.filter(|b| !b.is_empty()) else {
                return Err(anyhow!(
                    "dst_bucket is required for upload job when storage backend is S3"
                ));
            };
            if dst_bucket.contains('/') {
                return Err(anyhow!("Invalid dst_bucket: contains '/'"));
            }
            if is_pure_ascii_digits(&dst_bucket) {
                return Err(anyhow!(
                    "Invalid dst_bucket: numeric-only bucket is not allowed"
                ));
            }

            state
                .storage_manager
                .upload_directory_to_bucket(&dst_bucket, &upload_path, "videos")
                .await
                .map(|_| {
                    _ = std::fs::remove_dir_all(upload_path);
                })
                .inspect_err(|error| error!(%job_id, %error, "Failed to upload video directory"))
        })
    }

    fn wait_for_retry(&self) -> Option<Duration> {
        Some(Duration::from_secs(30))
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
