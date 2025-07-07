use std::collections::HashMap;
use std::sync::Arc;

use futures::lock::Mutex;

#[allow(dead_code)]
pub struct Bucket {
    inner: Arc<Mutex<BucketInner>>,
}

#[allow(dead_code)]
struct BucketInner {
    id: String,
    used: usize,
    capacity: usize,

    objects: HashMap<String, usize>, // object_id -> size
}

#[allow(dead_code)]
impl BucketInner {
    fn new(id: String, capacity: usize) -> Self {
        Self {
            id,
            used: 0,
            capacity,
            objects: HashMap::new(),
        }
    }

    pub fn load(inner: Self) -> Self {
        // TODO: check file system for existing objects
        // and recompute used size
        inner.recompute_used()
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn recompute_used(mut self) -> Self {
        let used = self.objects.values().sum::<usize>();
        self.used = used;
        self
    }

    pub fn used(&self) -> usize {
        self.used
    }

    pub fn available(&self) -> usize {
        self.capacity - self.used
    }

    pub fn add_object(&mut self, object_id: String, size: usize) -> bool {
        if self.used + size <= self.capacity {
            self.objects.insert(object_id, size);
            self.used += size;
            true
        } else {
            false
        }
    }

    pub fn remove_object(&mut self, object_id: &str) -> Option<usize> {
        if let Some(size) = self.objects.remove(object_id) {
            self.used -= size;
            Some(size)
        } else {
            None
        }
    }
}
