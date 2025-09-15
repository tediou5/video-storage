use crate::app_state::AppState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::error;

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

    pub fn gen_task(&self, state: AppState) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        let this = self.clone();
        tokio::spawn(async move {
            let job_id = this.job.id().to_string();
            crate::spawn_hls_job(this, state.videos_dir(), state.temp_dir())
                .await
                .inspect_err(|error| error!(%job_id, %error, "Failed to process video"))
        })
    }
}
