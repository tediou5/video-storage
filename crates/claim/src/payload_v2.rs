use serde::{Deserialize, Serialize};
use xorf::BinaryFuse16;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimPayloadV1 {
    /// Expiration time in Unix timestamp
    pub exp_unix: u32,
    /// Not-before time in Unix timestamp
    pub nbf_unix: u32,
    /// Assets filter map (job_ids) that this claim grants access to
    pub assets_filter: BinaryFuse16,
    /// Time window length in seconds (0 = unlimited)
    pub window_len_sec: u16,
    /// Maximum bandwidth in kbps (0 = unlimited)
    pub max_kbps: u16,
    /// Maximum concurrent connections (0 = unlimited)
    pub max_concurrency: u16,
    /// Allowed video widths/resolutions (empty = all widths allowed)
    pub allowed_widths: Vec<u16>,
}

#[allow(dead_code)]
fn new_filter(assets: &[String]) -> anyhow::Result<BinaryFuse16> {
    BinaryFuse16::try_from(assets)
        .map_err(|error| anyhow::anyhow!("Failed to create xorf filter: {error}",))
}
