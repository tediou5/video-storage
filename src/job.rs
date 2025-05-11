use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::error;

pub(crate) const JOB_FILE: &str = "pending-jobs.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Job {
    pub(crate) id: String,
    pub(crate) crf: u8, // 0-63
}

impl Job {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}

pub(crate) struct JobGenerator {
    pub(crate) job: Job,
    upload_path: PathBuf,
}

impl JobGenerator {
    pub(crate) fn new(job: Job, upload_path: PathBuf) -> Self {
        Self { job, upload_path }
    }

    pub(crate) fn id(&self) -> &str {
        self.job.id()
    }

    pub(crate) fn gen_task(&self) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        let upload_path = self.upload_path.clone();
        let job = self.job.clone();
        tokio::spawn(async move {
            crate::spawn_hls_job(upload_path, job.id().to_string())
                .await
                .inspect_err(|error| error!(job_id = %job.id(), %error, "Failed to process video"))
        })
    }
}
