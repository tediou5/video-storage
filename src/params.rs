use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub(crate) struct UploadResponse {
    pub(crate) job_id: String,
    pub(crate) message: String,
}

#[derive(Deserialize)]
pub(crate) struct UploadParams {
    /// ?filename=your_video.mp4
    pub(crate) filename: String,
}
