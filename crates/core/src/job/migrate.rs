use crate::app_state::AppState;
use crate::job::convert::RESOLUTIONS;
use crate::job::{FailureJob, Job, JobKind, MIGRATE_KIND};
use anyhow::{Context, Result};
use futures::TryStreamExt as _;
use opendal::EntryMode;
use opendal::Operator;
use opendal::options;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::task::JoinHandle as TokioJoinHandle;
use tokio_util::compat::{FuturesAsyncReadCompatExt as _, FuturesAsyncWriteCompatExt as _};
use tracing::info;

const MIGRATE_STREAM_CONCURRENCY: usize = 1;
const MIGRATE_STREAM_CHUNK_SIZE: usize = 8 * 1024 * 1024;
const MIGRATE_PROGRESS_LOG_INTERVAL: u64 = 100;
const MAX_RETRIES: u8 = 5;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MigrateStats {
    pub objects_total: u64,
    pub objects_copied: u64,
    pub bytes_copied: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MigrateJob {
    pub id: String,
    pub src_bucket: String,
    pub dst_bucket: String,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub widths: Vec<u16>,
    #[serde(default)]
    #[serde(skip)]
    pub retry_times: Arc<AtomicU8>,
}

impl MigrateJob {
    pub fn new(id: String, src_bucket: String, dst_bucket: String, widths: Vec<u16>) -> Self {
        Self {
            id,
            src_bucket,
            dst_bucket,
            widths,
            retry_times: Arc::new(AtomicU8::new(0)),
        }
    }

    pub fn resolved_widths(widths: Option<Vec<u16>>) -> Vec<u16> {
        widths.unwrap_or_else(Self::default_widths)
    }

    pub fn default_widths() -> Vec<u16> {
        RESOLUTIONS
            .iter()
            .map(|scale| u16::try_from(scale.width()).expect("resolution width must fit in u16"))
            .collect()
    }

    fn object_prefixes(&self) -> Vec<String> {
        let mut prefixes = Vec::with_capacity(self.widths.len() + 1);
        prefixes.push(format!("videos/{}", self.id));
        for width in &self.widths {
            prefixes.push(format!("videos/{width}/{}", self.id));
        }
        prefixes
    }

    async fn run(&self, state: &AppState) -> Result<MigrateStats> {
        let src_op = state
            .storage_manager
            .operator_for_bucket(&self.src_bucket)
            .with_context(|| format!("build source operator for bucket `{}`", self.src_bucket))?;
        let dst_op = state
            .storage_manager
            .operator_for_bucket(&self.dst_bucket)
            .with_context(|| {
                format!(
                    "build destination operator for bucket `{}`",
                    self.dst_bucket
                )
            })?;

        let keys = self.list_object_keys(&src_op).await?;
        let object_count = u64::try_from(keys.len()).expect("object count must fit in u64");
        info!(
            job_id = %self.id,
            src_bucket = %self.src_bucket,
            dst_bucket = %self.dst_bucket,
            object_count,
            "Migrate source objects discovered"
        );

        let mut stats = MigrateStats {
            objects_total: object_count,
            ..MigrateStats::default()
        };

        for key in keys {
            let meta = src_op.stat(&key).await.with_context(|| {
                format!(
                    "stat source object `{key}` after copying {}/{} objects ({} bytes)",
                    stats.objects_copied, stats.objects_total, stats.bytes_copied
                )
            })?;
            let copied = copy_object_streaming(&src_op, &dst_op, &key, meta.content_type())
                .await
                .with_context(|| {
                    format!(
                        "copy source object `{key}` after copying {}/{} objects ({} bytes)",
                        stats.objects_copied, stats.objects_total, stats.bytes_copied
                    )
                })?;

            stats.objects_copied += 1;
            stats.bytes_copied += copied;

            if stats.objects_copied == stats.objects_total
                || stats
                    .objects_copied
                    .is_multiple_of(MIGRATE_PROGRESS_LOG_INTERVAL)
            {
                info!(
                    job_id = %self.id,
                    src_bucket = %self.src_bucket,
                    dst_bucket = %self.dst_bucket,
                    objects_copied = stats.objects_copied,
                    objects_total = stats.objects_total,
                    bytes_copied = stats.bytes_copied,
                    "Migrate progress"
                );
            }
        }

        Ok(stats)
    }

    async fn list_object_keys(&self, src_op: &Operator) -> Result<BTreeSet<String>> {
        let mut keys = BTreeSet::new();

        for prefix in self.object_prefixes() {
            let mut lister = src_op
                .lister_options(
                    &prefix,
                    options::ListOptions {
                        recursive: true,
                        ..Default::default()
                    },
                )
                .await
                .with_context(|| format!("list source prefix `{prefix}`"))?;

            while let Some(entry) = lister
                .try_next()
                .await
                .with_context(|| format!("iterate source prefix `{prefix}`"))?
            {
                if entry.metadata().mode() != EntryMode::FILE {
                    continue;
                }
                let path = entry.path();
                let Some(rest) = path.strip_prefix(&prefix) else {
                    continue;
                };
                if !(rest.starts_with('.') || rest.starts_with('-')) {
                    continue;
                }
                let _ = keys.insert(path.to_string());
            }
        }

        Ok(keys)
    }
}

impl Job for MigrateJob {
    fn kind(&self) -> JobKind {
        MIGRATE_KIND
    }

    fn need_permit(&self) -> usize {
        1
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn gen_job(&self, state: AppState) -> TokioJoinHandle<anyhow::Result<()>> {
        let job = self.clone();
        tokio::spawn(async move {
            let stats = job.run(&state).await.with_context(|| {
                format!(
                    "migrate job `{}` from `{}` to `{}` failed",
                    job.id, job.src_bucket, job.dst_bucket
                )
            })?;

            info!(
                job_id = %job.id,
                src_bucket = %job.src_bucket,
                dst_bucket = %job.dst_bucket,
                objects_total = stats.objects_total,
                objects_copied = stats.objects_copied,
                bytes_copied = stats.bytes_copied,
                "Migrate job completed successfully"
            );
            state.call_webhook(&job.id, MIGRATE_KIND, "completed").await;

            Ok(())
        })
    }

    fn wait_for_retry(&self) -> Option<Duration> {
        let retry_times = self.retry_times.load(Ordering::Acquire);
        if retry_times < MAX_RETRIES {
            self.retry_times.store(retry_times + 1, Ordering::Release);
            Some(Duration::from_secs(30))
        } else {
            None
        }
    }

    fn on_final_failure(&self) -> FailureJob {
        FailureJob::new(self.id.clone(), self.kind(), Vec::new())
    }
}

async fn copy_object_streaming(
    src: &Operator,
    dst: &Operator,
    key: &str,
    content_type: Option<&str>,
) -> Result<u64> {
    let reader = src
        .reader_with(key)
        .chunk(MIGRATE_STREAM_CHUNK_SIZE)
        .concurrent(MIGRATE_STREAM_CONCURRENCY)
        .await?;
    let mut r = reader.into_futures_async_read(..).await?.compat();

    let mut writer = dst
        .writer_with(key)
        .chunk(MIGRATE_STREAM_CHUNK_SIZE)
        .concurrent(MIGRATE_STREAM_CONCURRENCY);
    if let Some(content_type) = content_type {
        writer = writer.content_type(content_type);
    }
    let mut w = writer.await?.into_futures_async_write().compat_write();

    let copied = tokio::io::copy(&mut r, &mut w).await?;
    use tokio::io::AsyncWriteExt as _;
    w.shutdown().await?;

    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_widths_use_defaults_when_omitted() {
        assert_eq!(
            MigrateJob::resolved_widths(None),
            MigrateJob::default_widths()
        );
    }

    #[test]
    fn migrate_job_retries_are_bounded() {
        let job = MigrateJob::new("video123".into(), "src".into(), "dst".into(), vec![480]);

        for _ in 0..MAX_RETRIES {
            assert_eq!(job.wait_for_retry(), Some(Duration::from_secs(30)));
        }
        assert_eq!(job.wait_for_retry(), None);
    }
}
