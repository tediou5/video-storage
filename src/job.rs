use crate::app_state::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::error;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct UploadJob {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
}

impl UploadJob {
    #[allow(unused)]
    pub(crate) fn new(id: String, path: PathBuf) -> Self {
        Self { id, path }
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ConvertJob {
    pub(crate) id: String,
    pub(crate) crf: u8, // 0-63
}

impl ConvertJob {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Clone, Debug)]
pub(crate) struct JobGenerator {
    pub(crate) job: ConvertJob,
    pub(crate) upload_path: PathBuf,
}

impl JobGenerator {
    pub(crate) fn new(job: ConvertJob, upload_path: PathBuf) -> Self {
        Self { job, upload_path }
    }

    pub(crate) fn id(&self) -> &str {
        self.job.id()
    }

    pub(crate) fn gen_task(&self, state: AppState) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        let this = self.clone();
        tokio::spawn(async move {
            let job_id = this.job.id().to_string();
            crate::spawn_hls_job(this, state.videos_dir(), state.temp_dir())
                .await
                .inspect_err(|error| error!(%job_id, %error, "Failed to process video"))
        })
    }
}
