use crate::app_state::AppState;
use crate::job::{FailureJob, Job, JobKind};
use crate::{ConvertJob, UploadJob};
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::cmp::Ordering;
use std::fmt;
use std::ops::Deref;
use std::ptr::NonNull;
use std::time::Duration;
use tokio::task::JoinHandle as TokioJoinHandle;

pub struct SerdeAbleRawJob {
    raw: RawJob,
    serializer: fn(&RawJob) -> JsonValue,
}

impl SerdeAbleRawJob {
    pub fn new<J>(job: J) -> Self
    where
        J: Job + Serialize,
    {
        let raw = RawJob::new(job);
        let serializer = |raw: &RawJob| {
            let job = unsafe {
                (raw.data.as_ptr() as *const J)
                    .as_ref()
                    .expect("RawJob ptr must can be deref")
            };
            serde_json::to_value(job).expect("Failed to serialize job")
        };

        SerdeAbleRawJob { raw, serializer }
    }

    pub fn raw(&self) -> RawJob {
        self.raw.clone()
    }

    pub fn to_json(&self) -> JsonValue {
        (self.serializer)(&self.raw)
    }

    pub fn from_json(value: JsonValue) -> Result<Self, serde_json::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum DeserAbleJob {
            Convert(ConvertJob),
            Upload(UploadJob),
        }

        let job = serde_json::from_value::<DeserAbleJob>(value)?;
        Ok(match job {
            DeserAbleJob::Convert(job) => SerdeAbleRawJob::new(job),
            DeserAbleJob::Upload(job) => SerdeAbleRawJob::new(job),
        })
    }
}

impl Deref for SerdeAbleRawJob {
    type Target = RawJob;

    fn deref(&self) -> &Self::Target {
        &self.raw
    }
}

impl Clone for SerdeAbleRawJob {
    fn clone(&self) -> Self {
        SerdeAbleRawJob {
            raw: self.raw.clone(),
            serializer: self.serializer,
        }
    }
}

impl Ord for SerdeAbleRawJob {
    fn cmp(&self, other: &Self) -> Ordering {
        self.raw.cmp(&other.raw)
    }
}

impl PartialOrd for SerdeAbleRawJob {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for SerdeAbleRawJob {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl Eq for SerdeAbleRawJob {}

impl fmt::Debug for SerdeAbleRawJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawJob")
            .field("kind", &self.kind())
            .field("id", &self.id())
            .finish()
    }
}

pub struct RawJob {
    data: NonNull<()>,
    vtable: &'static VTable,

    kind: JobKind,
    need_permit: bool,
}

unsafe impl Send for RawJob {}

unsafe impl Sync for RawJob {}

impl RawJob {
    pub fn new<J: Job>(job: J) -> Self {
        let kind = job.kind();
        let need_permit = job.need_permit();
        let vtable = &VTable {
            id: |this: *const ()| unsafe { &*(this as *const J) }.id() as *const str,
            gen_job: |this: *const (), state: AppState| {
                unsafe { &*(this as *const J) }.gen_job(state)
            },
            next_job: |this: *const (), state: AppState| unsafe {
                let ptr = (&*(this as *const J)).next_job(state);
                ptr.map(RawJob::new)
            },
            wait_for_retry: |this: *const ()| unsafe { (&*(this as *const J)).wait_for_retry() },
            on_final_failure: |this: *const ()| unsafe {
                (&*(this as *const J)).on_final_failure()
            },

            clone: |this: *const ()| unsafe {
                let ptr = (&*(this as *const J)).clone();
                RawJob::new(ptr)
            },
            drop: |this: *mut ()| drop(unsafe { Box::from_raw(this.cast::<J>()) }),
        };
        let data = unsafe { NonNull::new_unchecked(Box::into_raw(Box::new(job))).cast() };

        Self {
            data,
            vtable,

            kind,
            need_permit,
        }
    }
}

impl Job for RawJob {
    fn kind(&self) -> JobKind {
        self.kind
    }

    fn need_permit(&self) -> bool {
        self.need_permit
    }

    fn id(&self) -> &str {
        unsafe {
            (self.vtable.id)(self.data.as_ptr())
                .as_ref()
                .expect("RawJob must has id")
        }
    }

    fn gen_job(&self, state: AppState) -> TokioJoinHandle<anyhow::Result<()>> {
        (self.vtable.gen_job)(self.data.as_ptr(), state)
    }

    fn next_job(&self, state: AppState) -> Option<impl Job> {
        (self.vtable.next_job)(self.data.as_ptr(), state)
    }

    fn wait_for_retry(&self) -> Option<Duration> {
        (self.vtable.wait_for_retry)(self.data.as_ptr())
    }

    fn on_final_failure(&self) -> FailureJob {
        (self.vtable.on_final_failure)(self.data.as_ptr())
    }
}

impl<J: Job + Serialize> From<J> for RawJob {
    fn from(job: J) -> Self {
        RawJob::new(job)
    }
}

impl Clone for RawJob {
    fn clone(&self) -> Self {
        (self.vtable.clone)(self.data.as_ptr())
    }
}

impl Drop for RawJob {
    fn drop(&mut self) {
        unsafe { (self.vtable.drop)(self.data.as_ptr()) }
    }
}

// For use in `BTreeSet` or as `StreamMap` keys, we need comparison traits.
// We delegate these to the job's ID, which is assumed to be unique.
impl PartialEq for RawJob {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Eq for RawJob {}

impl PartialOrd for RawJob {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RawJob {
    fn cmp(&self, other: &Self) -> Ordering {
        self.id().cmp(other.id())
    }
}

impl fmt::Debug for RawJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawJob")
            .field("kind", &self.kind())
            .field("id", &self.id())
            .finish()
    }
}

struct VTable {
    id: fn(*const ()) -> *const str,
    gen_job: fn(*const (), AppState) -> TokioJoinHandle<anyhow::Result<()>>,
    next_job: fn(*const (), AppState) -> Option<RawJob>,
    wait_for_retry: fn(*const ()) -> Option<Duration>,
    on_final_failure: fn(*const ()) -> FailureJob,

    clone: fn(*const ()) -> RawJob,
    drop: unsafe fn(*mut ()),
}
