use anyhow::{Context, Result, anyhow};
use async_stream::try_stream;
use futures::StreamExt;
use opendal::services::{Fs, S3};
use opendal::{Operator, layers::RetryLayer};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio_util::compat::FuturesAsyncWriteCompatExt as _;
use tracing::{error, info};

/// Storage configuration
#[derive(Clone, Debug)]
pub struct StorageConfig {
    pub backend: StorageBackend,
    pub workspace: PathBuf,
}

#[derive(Clone, Debug)]
pub enum StorageBackend {
    Local,
    S3 {
        bucket: String,
        endpoint: Option<String>,
        region: Option<String>,
        access_key_id: String,
        secret_access_key: String,
    },
}

/// Storage manager to handle different storage backends
#[derive(Clone)]
pub struct StorageManager {
    operator: Operator,
    config: StorageConfig,
}

impl StorageManager {
    pub async fn new(config: StorageConfig) -> Result<Self> {
        let operator = match &config.backend {
            StorageBackend::Local => build_fs_operator(&config.workspace)?,
            StorageBackend::S3 {
                bucket,
                endpoint,
                region,
                access_key_id,
                secret_access_key,
            } => build_s3_operator(
                bucket,
                endpoint.as_deref(),
                region.as_deref(),
                access_key_id,
                secret_access_key,
            )?,
        };

        Ok(Self { operator, config })
    }

    #[allow(unused)]
    pub fn operator(&self) -> &Operator {
        &self.operator
    }

    pub fn is_remote(&self) -> bool {
        matches!(self.config.backend, StorageBackend::S3 { .. })
    }

    pub async fn upload_directory(&self, local_path: PathBuf, remote_prefix: &str) -> Result<()> {
        if !self.is_remote() {
            // For local storage, no upload needed
            info!("Using local storage, skipping upload");
            return Ok(());
        }

        info!(
            local_path = ?local_path,
            remote_prefix = %remote_prefix,
            "Starting directory upload to remote storage"
        );

        upload_tree_streaming(
            self.operator.clone(),
            local_path,
            remote_prefix,
            8, // concurrency
        )
        .await
    }
}

fn build_fs_operator(root: &Path) -> Result<Operator> {
    info!(root = ?root, "Building filesystem operator");

    let mut builder = Fs::default();
    builder = builder.root(
        root.to_str()
            .ok_or_else(|| anyhow!("Invalid root path: {:?}", root))?,
    );

    Ok(Operator::new(builder)?
        .layer(RetryLayer::new().with_max_times(3))
        .finish())
}

fn build_s3_operator(
    bucket: &str,
    endpoint: Option<&str>,
    region: Option<&str>,
    access_key_id: &str,
    secret_access_key: &str,
) -> Result<Operator> {
    info!(
        bucket = %bucket,
        endpoint = ?endpoint,
        region = ?region,
        "Building S3 operator"
    );

    let mut builder = S3::default();
    builder = builder.bucket(bucket);
    builder = builder.access_key_id(access_key_id);
    builder = builder.secret_access_key(secret_access_key);

    if let Some(region) = region {
        builder = builder.region(region);
    }

    if let Some(endpoint) = endpoint {
        builder = builder.endpoint(endpoint);
    }

    Ok(Operator::new(builder)?
        .layer(RetryLayer::new().with_max_times(3))
        .finish())
}

fn walk_files_stream(root: PathBuf) -> impl futures::Stream<Item = Result<PathBuf>> {
    try_stream! {
        let mut q = VecDeque::new();
        q.push_back(root);
        while let Some(dir) = q.pop_front() {
            let mut rd = tokio::fs::read_dir(&dir).await
                .with_context(|| format!("read_dir {}", dir.display()))?;
            while let Some(entry) = rd.next_entry().await? {
                let path = entry.path();
                let ft = entry.file_type().await?;
                if ft.is_dir() {
                    q.push_back(path);
                } else if ft.is_file() {
                    yield path;
                }
            }
        }
    }
}

fn to_s3_key(local_root: &Path, file: &Path, prefix: &str) -> Result<String> {
    let rel = file
        .strip_prefix(local_root)
        .with_context(|| format!("{} not under {}", file.display(), local_root.display()))?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    Ok(if prefix.is_empty() {
        rel
    } else {
        format!("{}/{}", prefix.trim_end_matches('/'), rel)
    })
}

pub async fn upload_tree_streaming(
    op: Operator,
    local_root: PathBuf,
    prefix: &str,
    concurrency: usize,
) -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let op = Arc::new(op);
    let prefix = prefix.to_string();

    let success_count = Arc::new(AtomicUsize::new(0));
    let error_count = Arc::new(AtomicUsize::new(0));

    let success_count_clone = success_count.clone();
    let error_count_clone = error_count.clone();

    walk_files_stream(local_root.clone())
        .for_each_concurrent(concurrency, move |rpath| {
            let op = op.clone();
            let root = local_root.clone();
            let prefix = prefix.clone();
            let success_count = success_count_clone.clone();
            let error_count = error_count_clone.clone();

            async move {
                match rpath {
                    Ok(path) => {
                        if let Err(err) = upload_one(&op, &root, &path, &prefix).await {
                            error!(?path, ?err, "ERR upload");
                            error_count.fetch_add(1, Ordering::Relaxed);
                        } else {
                            success_count.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(err) => {
                        error!(?err, "ERR walk");
                        error_count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
        .await;

    let final_success = success_count.load(Ordering::Relaxed);
    let final_errors = error_count.load(Ordering::Relaxed);

    if final_errors > 0 {
        error!(
            success = final_success,
            errors = final_errors,
            "Upload completed with errors"
        );
        Err(anyhow!("Upload failed with {} errors", final_errors))
    } else {
        info!(success = final_success, "Upload completed successfully");
        Ok(())
    }
}

async fn upload_one(op: &Operator, local_root: &Path, file: &Path, prefix: &str) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let key = to_s3_key(local_root, file, prefix)?;
    let mime = mime_guess::from_path(file)
        .first_or_octet_stream()
        .to_string();

    let started = Instant::now();
    let mut r = tokio::fs::File::open(file).await?;
    let mut w = op
        .writer_with(&key)
        .content_type(&mime)
        .concurrent(8) // 并发分片（S3: multipart）
        .chunk(8 * 1024 * 1024) // 分片大小 8MiB（S3 最小 5MiB）
        .await?
        .into_futures_async_write()
        .compat_write();

    let copied = tokio::io::copy(&mut r, &mut w).await?;

    // Ensure all data is written by closing the writer
    w.shutdown().await?;

    let dur = started.elapsed();
    info!(
        %key,
        ?file,
        bytes_copied=copied,
        elapsed_ms=%dur.as_millis(),
        content_type=%mime,
        "OK upload"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_to_s3_key() {
        let local_root = PathBuf::from("/tmp/videos");
        let file = PathBuf::from("/tmp/videos/test/file.mp4");
        let key = to_s3_key(&local_root, &file, "videos").unwrap();
        assert_eq!(key, "videos/test/file.mp4");
    }

    #[tokio::test]
    async fn test_walk_files() {
        let local_root = std::path::PathBuf::from("videos/t3");

        let root = local_root.clone();
        let videos_path = walk_files_stream(local_root)
            .filter_map(|file| {
                let root_path = root.clone();

                async move {
                    // Process each file here
                    let file = file.ok()?;
                    println!("Processing file: {file:?}");
                    to_s3_key(&root_path, &file, "videos").ok()
                }
            })
            .collect::<Vec<_>>()
            .await;
        println!("videos_paths: {videos_path:?}");
    }
}
