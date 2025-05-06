use crate::{JobGenerator, StreamMap};

use std::collections::BTreeSet;

use async_channel::{Receiver, Sender, unbounded};
use futures::StreamExt;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

pub(crate) struct JobTask {
    pub(crate) id: String,
    pub(crate) generator: JobGenerator,
}

pub(crate) struct AppState {
    pub(crate) job_set: TokioMutex<BTreeSet<String>>,
    pub(crate) job_tx: Sender<JobTask>,
}

impl AppState {
    pub(crate) fn new() -> Self {
        let (tx, rx) = unbounded();
        let this = Self {
            job_set: TokioMutex::new(BTreeSet::new()),
            job_tx: tx,
        };

        this.handle_jobs(rx);
        this
    }

    fn handle_jobs(&self, rx: Receiver<JobTask>) {
        info!("Job handler started");
        let tx = self.job_tx.clone();

        tokio::spawn(async move {
            let rx = std::pin::pin!(rx);
            let mut rx = rx.fuse();

            let mut jobs: StreamMap<'_, String, Result<(), JobTask>> = StreamMap::default();
            loop {
                futures::select! {
                    job = rx.select_next_some() => {
                        let JobTask {id, generator} = job;
                        let id_c = id.clone();
                        let task = async move {
                            generator.gen_task().await.expect("Tokio task Join failed").map_err(|error| {
                                error!(%error, %id_c, "Job process failed, retry later");
                                JobTask {
                                    id: id_c,
                                    generator,
                                }
                            })
                        };
                        if !jobs.add_if_not_in_progress(id.clone(), Box::pin(task)) {
                            warn!("Job {id} already in-progress, skipping");
                            continue;
                        }
                    }
                    (id, result) = jobs.select_next_some() => {
                        let Err(job) = result else {
                            info!("Job {id} completed successfully");
                            continue;
                        };

                        warn!(%id, "Retry job");
                        _ = tx.send(job);
                    }
                }
            }
        });
    }
}
