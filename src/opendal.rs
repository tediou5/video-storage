use anyhow::{Context, Result};
use async_stream::try_stream;
use futures::{AsyncWriteExt as _, Stream, StreamExt};
use opendal::Operator;
use opendal::layers::{CapabilityCheckLayer, MimeGuessLayer, RetryLayer};
use opendal::services::S3;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio_util::compat::FuturesAsyncWriteCompatExt as _;
use tracing::{error, info};
// use tracing_subscriber::{EnvFilter, fmt};

#[derive(Default)]
struct Stats {
    ok: AtomicUsize,
    skip: AtomicUsize,
    err: AtomicUsize,
    bytes_uploaded: AtomicU64,
}

fn build_s3_operator() -> Result<Operator> {
    // 从环境变量读取：
    //   S3_BUCKET, AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY
    // 可选：AWS_REGION, S3_ENDPOINT
    let mut b = S3::default();
    // b.bucket(std::env::var("S3_BUCKET")?);
    // b.access_key_id(std::env::var("AWS_ACCESS_KEY_ID")?);
    // b.secret_access_key(std::env::var("AWS_SECRET_ACCESS_KEY")?);
    // if let Ok(region) = std::env::var("AWS_REGION") {
    //     b.region(region);
    // }
    // if let Ok(endpoint) = std::env::var("S3_ENDPOINT") {
    //     b.endpoint(endpoint);
    // }
    Ok(Operator::new(b)?.finish())
}

/// 异步 BFS 遍历，产出所有文件路径
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

/// 本地 -> S3 key（prefix + 相对路径，分隔符标准化为 /）
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

async fn upload_tree_streaming(
    op: Operator,
    local_root: PathBuf,
    prefix: &str,
    concurrency: usize,
    stats: Arc<Stats>,
) -> Result<()> {
    let op = Arc::new(op);
    let prefix = prefix.to_string();

    walk_files_stream(local_root.clone())
        .for_each_concurrent(concurrency, move |rpath| {
            let op = op.clone();
            let root = local_root.clone();
            let prefix = prefix.clone();
            let stats = stats.clone();

            async move {
                match rpath {
                    Ok(path) => {
                        if let Err(e) = upload_one(&op, &root, &path, &prefix, &stats).await {
                            error!(path=%path.display(), err=%err_chain(&e), "ERR upload");
                            stats.err.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(e) => {
                        error!(err=%err_chain(&e), "ERR walk");
                        stats.err.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
        .await;

    Ok(())
}

/// 单文件上传：已存在且大小一致则跳过；writer_with 设置 header；流式复制并统计
async fn upload_one(
    op: &Operator,
    local_root: &Path,
    file: &Path,
    prefix: &str,
    stats: &Stats,
) -> Result<()> {
    let key = to_s3_key(local_root, file, prefix)?;
    let meta_local = tokio::fs::metadata(file).await?;
    let local_len = meta_local.len();

    // 若远端已存在且大小一致 → SKIP
    if let Ok(meta_remote) = op.stat(&key).await {
        if meta_remote.content_length() == local_len {
            info!(key=%key, bytes=%human_bytes(local_len as u64), "SKIP exists(same size)");
            stats.skip.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
    }

    // 选择 MIME 与缓存策略
    let mime = mime_guess::from_path(&file)
        .first_or_octet_stream()
        .to_string();

    // 开始上传
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

    let dur = started.elapsed();
    stats.ok.fetch_add(1, Ordering::Relaxed);
    stats
        .bytes_uploaded
        .fetch_add(copied as u64, Ordering::Relaxed);

    info!(
        key=%key,
        bytes=%human_bytes(copied),
        elapsed_ms=%dur.as_millis(),
        speed=%throughput(copied, dur),
        content_type=%mime,
        "OK upload"
    );
    Ok(())
}

fn human_bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n == 0 {
        return "0 B".into();
    }
    let i = (n as f64).log(K).floor() as usize;
    let v = n as f64 / K.powi(i as i32);
    format!("{:.2} {}", v, UNITS[i])
}

fn throughput(bytes: u64, dur: Duration) -> String {
    if dur.is_zero() {
        return "-".into();
    }
    let bps = (bytes as f64) / dur.as_secs_f64();
    format!("{}/s", human_bytes(bps as u64))
}

/// 友好的错误链打印
fn err_chain(e: &anyhow::Error) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(": ");
        s.push_str(&c.to_string());
        cur = c.source();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test() {
        let local_root = std::path::PathBuf::from("videos/t3");

        let root = local_root.clone();
        let vidoes_path = walk_files_stream(local_root)
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
        panic!("videos_paths: {vidoes_path:?}");
    }
}
