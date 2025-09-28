use crate::job::raw::SerdeAbleRawJob;
use crate::job::{Job, RawJob};
use futures::channel::mpsc::UnboundedSender;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

const PENDING_FILE: &str = "pending-jobs.json";

/// Manages a set of jobs of a specific type `J`, including persistence to a file.
#[derive(Debug, Clone)]
pub struct JobSetManager {
    path: PathBuf,
    pub jobs: Arc<TokioMutex<Vec<SerdeAbleRawJob>>>,
}

impl JobSetManager {
    /// Creates a new `JobSetManager` for a specific job type.
    /// It loads any pending jobs from the file specified in the `Job` trait.
    pub fn new(workspace: &Path, tx: &UnboundedSender<RawJob>) -> anyhow::Result<Self> {
        let path = workspace.join(PENDING_FILE);
        let jobs = if path.exists() {
            let content = fs::read_to_string(&path)?;

            let jobs: Vec<JsonValue> = serde_json::from_str(&content)
                .inspect_err(|error| {
                    warn!(?error, ?path, "Failed to parse job file.");
                })
                .unwrap_or_default();
            jobs.into_iter()
                .map(SerdeAbleRawJob::from_json)
                .collect::<Result<Vec<_>, _>>()
                .inspect_err(|error| {
                    warn!(?error, ?path, "Failed to parse job json.");
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        info!(
            count = jobs.len(),
            file = %path.display(),
            "Initialize job manager"
        );

        for job in jobs.iter() {
            info!(job_id = %job.id(), kind = %job.kind(), "Loading pending job");
            _ = tx.unbounded_send(job.raw());
        }

        Ok(Self {
            path,
            jobs: Arc::new(TokioMutex::new(jobs)),
        })
    }

    /// Atomically saves the current set of jobs to its JSON file.
    async fn save(&self, jobs: &[SerdeAbleRawJob]) -> anyhow::Result<()> {
        let values = jobs.iter().map(|job| job.to_json()).collect::<Vec<_>>();
        let content = serde_json::to_string(&values).expect("Failed to serialize jobs");
        tracing::debug!(path = %self.path.display(), ?content, "Saving jobs to file");

        Ok(tokio::fs::write(&self.path, content).await?)
    }

    /// Adds a job to the set and persists the change.
    pub async fn add<J>(&self, job: &J)
    where
        J: Job + Serialize,
    {
        let kind = job.kind();
        tracing::debug!(id = %job.id(), kind, "Adding job");
        let job = SerdeAbleRawJob::new(job.clone());
        let mut jobs = self.jobs.lock().await;

        // remove any existing job with the same ID
        jobs.retain(|j| j.id() != job.id());
        jobs.push(job);

        if let Err(error) = self.save(&jobs).await {
            error!(?kind, ?error, "Failed to save jobs file after adding a job");
        }
    }

    /// Removes a job from the set by its ID and persists the change.
    pub async fn remove(&self, id: &str) {
        tracing::debug!(id, "Removing job");
        let mut jobs = self.jobs.lock().await;
        jobs.retain(|j| j.id() != id);

        if let Err(error) = self.save(&jobs).await {
            error!(id, ?error, "Failed to save jobs file after removing a job");
        }
    }
}
