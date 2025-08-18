use serde::{Deserialize, Serialize};
use std::fs::Metadata;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use walkdir::DirEntry;

#[allow(dead_code)]
pub struct Bucket {
    inner: Arc<Mutex<BucketInner>>,
}

#[allow(dead_code)]
impl Bucket {
    pub async fn join<P: AsRef<Path>>(&mut self, path: P) -> BucketGuard {
        let inner = self.inner.clone();
        let path = inner.lock().await.path.join(path);
        BucketGuard { path, inner }
    }
}

pub trait BucketEntry {
    fn used(&self) -> usize;
}

fn entry_size(entry: DirEntry) -> usize {
    entry.metadata().as_ref().map_or(0, Metadata::len) as usize
}

impl<P: AsRef<Path>> BucketEntry for P {
    fn used(&self) -> usize {
        walkdir::WalkDir::new(self)
            .into_iter()
            .filter_map(Result::ok)
            .map(entry_size)
            .sum()
    }
}

#[allow(dead_code)]
pub struct BucketGuard {
    path: PathBuf,
    inner: Arc<Mutex<BucketInner>>,
}

#[allow(dead_code)]
impl Drop for BucketGuard {
    fn drop(&mut self) {
        let used = self.path.used();
        let mut inner = self.inner.blocking_lock();
        inner.used += used;
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize, Serialize)]
struct BucketInner {
    id: String,
    used: usize,
    capacity: usize,
    path: PathBuf,
}

#[allow(dead_code)]
impl BucketInner {
    fn new(id: String, capacity: usize, path: PathBuf) -> Self {
        Self {
            id,
            used: 0,
            capacity,
            path,
        }
    }

    fn load(inner: Self) -> Self {
        inner.recompute_used()
    }

    fn recompute_used(mut self) -> Self {
        self.used = BucketEntry::used(&self.path);
        self
    }

    fn available(&self) -> usize {
        self.capacity - self.used
    }
}
