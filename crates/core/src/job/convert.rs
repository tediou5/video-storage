use crate::app_state::AppState;
use crate::job::upload::UploadJob;
use crate::job::{Action, CONVERT_KIND, FailureJob, Job, JobKind, RawJob};
use ez_ffmpeg::Frame;
use ez_ffmpeg::container_info::get_duration_us;
use ez_ffmpeg::filter::frame_filter::FrameFilter;
use ez_ffmpeg::filter::frame_filter_context::FrameFilterContext;
use ez_ffmpeg::filter::frame_pipeline_builder::FramePipelineBuilder;
use ez_ffmpeg::{AVMediaType, Input};
use ez_ffmpeg::{FfmpegContext, Output};
use ffmpeg_next::{codec, format, media};
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle as TokioJoinHandle;
use tracing::{debug, error, info, warn};

pub const RESOLUTIONS: [Scale; 3] = [
    Scale::new(720, 1280),
    Scale::new(540, 960),
    Scale::new(480, 854),
];
/// HLS segment duration in seconds
pub const HLS_SEGMENT_DURATION: u16 = 4;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Scale {
    #[serde(rename = "w")]
    width: u32,
    #[serde(rename = "h")]
    height: u32,
}

impl Scale {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

impl From<&Scale> for (u32, u32) {
    fn from(scale: &Scale) -> Self {
        (scale.width, scale.height)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Scales(Vec<Scale>);

impl Scales {
    pub fn new() -> Self {
        Self(RESOLUTIONS.to_vec())
    }

    pub fn from_vec(scales: Vec<Scale>) -> Self {
        Self(scales)
    }

    pub fn add_scale(&mut self, width: u32, height: u32) {
        self.0.push(Scale::new(width, height));
    }

    pub fn skip_serialize(&self) -> bool {
        self.0.as_slice() == RESOLUTIONS || self.0.is_empty()
    }

    pub fn bandwidth(width: u32) -> u32 {
        match width {
            w if w > 720 => 5_000_000,
            w if w > 540 => 2_500_000,
            w if w > 480 => 1_500_000,
            _ => 1_000_000,
        }
    }
}

impl Deref for Scales {
    type Target = [Scale];

    fn deref(&self) -> &Self::Target {
        if self.0.is_empty() {
            &RESOLUTIONS
        } else {
            &self.0[..]
        }
    }
}

impl Default for Scales {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConvertJob {
    pub id: String,
    pub crf: u8, // 0-63

    #[serde(default)]
    #[serde(skip_serializing_if = "Scales::skip_serialize")]
    pub scales: Scales,

    #[serde(default)]
    #[serde(skip)]
    pub retry_times: Arc<AtomicU8>,
}

impl ConvertJob {
    pub fn new(id: String, crf: u8, scales: Scales) -> Self {
        Self {
            id,
            crf,
            scales,
            retry_times: Arc::new(AtomicU8::new(0)),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn crf(&self) -> u8 {
        self.crf
    }

    /// Generate the upload path from the state
    pub fn upload_path(&self, state: &AppState) -> PathBuf {
        state.uploads_dir().join(&self.id)
    }
}

impl Job for ConvertJob {
    fn kind(&self) -> JobKind {
        CONVERT_KIND
    }

    fn need_permit(&self) -> bool {
        true
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn gen_job(&self, state: AppState) -> TokioJoinHandle<anyhow::Result<()>> {
        let job = self.clone();
        tokio::spawn(async move {
            let temp_dir = state.temp_dir().join(format!("tmp-{}", job.id()));
            let out_dir = state.videos_dir().join(job.id());
            tokio::fs::create_dir_all(&temp_dir).await?;
            tokio::task::spawn_blocking(move || {
                let input_path = job.upload_path(&state);
                let sanitized_input = format!("/tmp/{}-sanitized.mp4", job.id());
                _ = std::fs::File::create(&sanitized_input)?;
                remux_av_only(input_path.to_str().unwrap(), &sanitized_input)
                    .inspect_err(|error| error!(?input_path, ?error, "remuxing input failed"))?;

                create_master_playlist(&job, &sanitized_input, &temp_dir)?;
                // Remove existing directory if it exists
                _ = std::fs::remove_dir_all(&out_dir);
                _ = std::fs::remove_file(sanitized_input);
                std::fs::rename(&temp_dir, &out_dir)?;
                // Cleanup original file
                _ = std::fs::remove_file(&input_path);
                info!(job_id = job.id(), "HLS conversion completed successfully");

                tokio::spawn(async move {
                    state
                        .call_webhook(job.id(), CONVERT_KIND, "completed")
                        .await;
                });

                Ok::<(), anyhow::Error>(())
            })
            .await??;

            Ok(())
        })
    }

    fn next_job(&self, state: AppState) -> Option<impl Job> {
        if state.storage_manager.operator().is_some() {
            let next = UploadJob::new(self.id.clone());

            let next_c = next.clone();
            let state_c = state.clone();
            tokio::spawn(async move { state_c.jobs_manager.add(&next_c).await });
            Some(RawJob::new(next))
        } else {
            None
        }
    }

    fn wait_for_retry(&self) -> Option<Duration> {
        let retry_times = self.retry_times.load(Ordering::Acquire);
        if retry_times < 5 {
            self.retry_times.store(retry_times + 1, Ordering::Release);
            Some(Duration::from_secs(15))
        } else {
            None
        }
    }

    fn on_final_failure(&self) -> FailureJob {
        FailureJob::new(
            self.id.clone(),
            self.kind(),
            vec![
                Action::Cleanup,
                Action::Webhook {
                    message: "Convert job failed after all retries".to_string(),
                },
            ],
        )
    }
}

/// Convert MP4 to a VP9 variant.
pub fn convert(
    job: &ConvertJob,
    input: &str,
    output: &Path,
    progress_callbacker: Arc<ProgressCallBacker>,
) -> anyhow::Result<()> {
    let ConvertJob {
        id: job_id,
        crf,
        scales,
        ..
    } = job;

    let outputs = scales
        .iter()
        .enumerate()
        .map(|(i, scale)| {
            let w = scale.width();
            let frame_pipeline: FramePipelineBuilder = AVMediaType::AVMEDIA_TYPE_AUDIO.into();
            let progress_filter = ProgressCallBackFilter::new(progress_callbacker.clone());
            let pipe = frame_pipeline.filter("progress", Box::new(progress_filter));

            let out_dir = output.join(w.to_string());
            std::fs::create_dir_all(&out_dir)?;

            let playlist_path = out_dir.join(format!("{job_id}.m3u8"));
            let output_str = out_dir.to_str().unwrap();
            let hls_segment_filename = format!("{output_str}/{job_id}-%03d.m4s");

            Ok(Output::from(playlist_path.to_str().unwrap())
                .add_frame_pipeline(pipe)
                .set_video_codec_opt("crf", crf.to_string())
                .add_stream_map("0:a?")
                .add_stream_map(format!("v{i}"))
                .set_audio_codec("libopus")
                .set_video_codec("libvpx-vp9")
                // hls settings
                .set_format("hls")
                .set_format_opt("hls_time", HLS_SEGMENT_DURATION.to_string())
                .set_format_opt("hls_segment_type", "fmp4")
                .set_format_opt("hls_playlist_type", "vod")
                .set_format_opt("hls_fmp4_init_filename", format!("{job_id}-init.mp4"))
                .set_format_opt("hls_segment_filename", hls_segment_filename))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let target_scales = scales.len();
    let (outs, scales_str): (String, String) = scales
        .iter()
        .map(From::from)
        .enumerate()
        .map(|(i, (w, h))| (format!("[out{i}]"), format!(";[out{i}]scale={w}:{h}[v{i}]")))
        .unzip();
    let graphs = format!("[0:v]split={target_scales}{outs}{scales_str}");
    debug!(%job_id, %crf, ?input, ?graphs, "Converting to HLS");

    let input = Input::from(input);
    let builder = FfmpegContext::builder().input(input).filter_desc(graphs);
    Ok(builder.outputs(outputs).build()?.start()?.wait()?)
}

/// Remux video: copy video and audio streams without re-encoding, skip subtitle streams
/// Equivalent to: ffmpeg -i in.mp4 -map 0:v -map 0:a? -c:v copy -c:a copy -dn out.mp4
fn remux_av_only(input: &str, output: &str) -> anyhow::Result<()> {
    debug!(?input, ?output, "remux_av_only: skip subtitle streams");
    let mut input_context = format::input(input)?;
    let mut output_context = format::output(output)?;
    let mut stream_mapping = vec![None; input_context.nb_streams() as usize];

    for (i, stream) in input_context.streams().enumerate() {
        let medium = stream.parameters().medium();

        // Only copy video and audio streams (skip subtitle and data streams)
        match medium {
            media::Type::Video | media::Type::Audio => {
                let mut output_stream = output_context.add_stream(codec::Id::None)?;
                output_stream.set_parameters(stream.parameters());
                output_stream.set_time_base(stream.time_base());

                stream_mapping[i] = Some(output_stream.index());
            }
            _ => {
                // Skip other stream types (subtitles, data, etc.)
                stream_mapping[i] = None;
            }
        }
    }

    output_context.write_header()?;

    for (stream, mut packet) in input_context.packets() {
        let stream_index = stream.index();

        if let Some(output_index) = stream_mapping[stream_index] {
            let out_stream = output_context.stream(output_index).unwrap();
            packet.set_stream(output_index);
            packet.rescale_ts(stream.time_base(), out_stream.time_base());

            packet.write_interleaved(&mut output_context)?;
        }
    }

    output_context.write_trailer()?;

    debug!(?input, ?output, "remux_av_only: done");

    Ok(())
}

pub fn create_master_playlist(job: &ConvertJob, input: &str, output: &Path) -> anyhow::Result<()> {
    let job_id = job.id();

    let total = get_duration_us(input).unwrap();
    info!(%job_id, "Duration: {total} us");

    let mut progress_callbacker = ProgressCallBacker::new(job_id.to_string());
    progress_callbacker.total_duration = total;

    // Retrieve the audio stream information
    let audio_info = ez_ffmpeg::stream_info::find_audio_stream_info(input).unwrap();
    if let Some(audio_info) = audio_info {
        if let ez_ffmpeg::stream_info::StreamInfo::Audio { time_base, .. } = audio_info {
            progress_callbacker.time_base = time_base;
            info!(%job_id, "Audio time base: {}/{}", time_base.num, time_base.den);
        }
    } else {
        warn!("Audio stream information not found");
    }

    convert(job, input, output, progress_callbacker.into())
        .inspect_err(|error| error!(?error, "Failed to convert to HLS with scales"))?;

    let master_playlist_path = output.join(format!("{job_id}.m3u8"));
    let mut file = std::fs::File::create(&master_playlist_path)?;

    let mut content = String::new();
    content.push_str("#EXTM3U\n");
    content.push_str("#EXT-X-VERSION:3\n");
    content.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");

    // #EXTM3U
    // #EXT-X-VERSION:3
    // #EXT-X-INDEPENDENT-SEGMENTS
    // Generate entries for each scale in the job
    for (w, h) in job.scales.iter().map(From::from) {
        let bandwidth = Scales::bandwidth(w);
        content.push_str(&format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth},RESOLUTION={w}x{h},CODECS=\"vp09.00.51.08.01.01.01.01.00,opus\"\n"
        ));
        content.push_str(&format!("{w}/{job_id}.m3u8\n"));
    }

    file.write_all(content.as_bytes())?;
    info!(%job_id, "Master playlist created at: {:?}", master_playlist_path);
    Ok(())
}

pub struct ProgressCallBacker {
    job_id: String,
    total_duration: i64,
    time_base: ez_ffmpeg::AVRational,
    last_report: Arc<Mutex<i64>>,
}

impl ProgressCallBacker {
    pub fn new(job_id: String) -> Self {
        Self {
            job_id,
            total_duration: 0,
            time_base: ez_ffmpeg::AVRational { num: 0, den: 0 },
            last_report: Arc::new(Mutex::new(0)),
        }
    }

    pub fn print_progress(&self, frame: &Frame) {
        if let Some(pts) = frame.pts() {
            // Check if the time base is valid.
            if self.time_base.den == 0 {
                warn!("The time base denominator is 0, and the time cannot be calculated.");
                return;
            }

            let mut last_reported = self.last_report.lock().unwrap();
            // Get the timestamp of the frame (in seconds).
            let time = pts * self.time_base.num as i64 * 100 / self.time_base.den as i64;
            let total = self.total_duration / 10_000;

            if time >= *last_reported + 1_000 {
                *last_reported = time;
                let time = time as f64 / 100.0;
                let total = total as f64 / 100.0;
                let progress = (time / total) * 100.0;
                let clamped = progress.clamp(0.0, 100.0);
                info!(
                    job_id = %self.job_id,
                    "Progress: {clamped:.2}% (Current: {time:.3}s / Total: {total:.2}s, PTS: {pts})"
                );
            }
        }
    }
}

pub struct ProgressCallBackFilter {
    progress_callback: Arc<ProgressCallBacker>,
}

impl ProgressCallBackFilter {
    pub fn new(progress_callback: Arc<ProgressCallBacker>) -> Self {
        Self { progress_callback }
    }
}

impl FrameFilter for ProgressCallBackFilter {
    fn media_type(&self) -> AVMediaType {
        AVMediaType::AVMEDIA_TYPE_AUDIO // Process audio frames
    }

    fn filter_frame(
        &mut self,
        frame: Frame,
        _ctx: &FrameFilterContext,
    ) -> Result<Option<Frame>, String> {
        unsafe {
            // Ensure the frame is valid and not empty
            if frame.as_ptr().is_null() || frame.is_empty() {
                return Ok(Some(frame)); // If invalid, simply return the frame as-is
            }
        }
        self.progress_callback.print_progress(&frame);
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn test_scale_construction_and_accessors() {
        let scale = Scale::new(1920, 1080);
        assert_eq!(scale.width(), 1920);
        assert_eq!(scale.height(), 1080);

        let scale2 = Scale::new(720, 1280);
        assert_eq!(scale2.width(), 720);
        assert_eq!(scale2.height(), 1280);
    }

    #[test]
    fn test_scale_serialization() {
        let scale = Scale::new(1920, 1080);
        let json = serde_json::to_string(&scale).unwrap();
        // Should use short field names w/h
        assert!(json.contains(r#""w":1920"#));
        assert!(json.contains(r#""h":1080"#));
    }

    #[test]
    fn test_scale_deserialization() {
        let json = r#"{"w":1920,"h":1080}"#;
        let scale: Scale = serde_json::from_str(json).unwrap();
        assert_eq!(scale.width(), 1920);
        assert_eq!(scale.height(), 1080);
    }

    #[test]
    fn test_scales_default() {
        let scales = Scales::new();
        assert_eq!(scales.0, RESOLUTIONS.to_vec());
        assert_eq!(&*scales, &RESOLUTIONS);
    }

    #[test]
    fn test_scales_empty_deref() {
        let scales = Scales::from_vec(Vec::new());
        // When empty, should deref to default RESOLUTIONS
        assert_eq!(&*scales, &RESOLUTIONS);
    }

    #[test]
    fn test_scales_custom_deref() {
        let custom_scales = vec![Scale::new(1920, 1080), Scale::new(1280, 720)];
        let scales = Scales::from_vec(custom_scales.clone());
        // When not empty, should deref to custom scales
        assert_eq!(&*scales, &custom_scales[..]);
    }

    #[test]
    fn test_scales_skip_serialize() {
        let default_scales = Scales::new();
        assert!(default_scales.skip_serialize());

        let empty_scales = Scales::from_vec(Vec::new());
        assert!(empty_scales.skip_serialize());

        let custom_scales = Scales::from_vec(vec![Scale::new(1920, 1080)]);
        assert!(!custom_scales.skip_serialize());
    }

    #[test]
    fn test_scales_bandwidth() {
        assert_eq!(Scales::bandwidth(800), 5_000_000);
        assert_eq!(Scales::bandwidth(720), 2_500_000);
        assert_eq!(Scales::bandwidth(600), 2_500_000);
        assert_eq!(Scales::bandwidth(540), 1_500_000);
        assert_eq!(Scales::bandwidth(500), 1_500_000);
        assert_eq!(Scales::bandwidth(480), 1_000_000);
        assert_eq!(Scales::bandwidth(300), 1_000_000);
    }

    #[test]
    fn test_convert_job_deserialize_legacy_without_scales() {
        // Test deserializing old JSON format without scales field - should use defaults
        let json = r#"{
            "id": "test-job-1",
            "crf": 23
        }"#;

        let job: ConvertJob = serde_json::from_str(json).unwrap();

        assert_eq!(job.id, "test-job-1");
        assert_eq!(job.crf, 23);
        // Should use default scales for backward compatibility
        assert_eq!(job.scales.len(), RESOLUTIONS.len());
        assert_eq!(&*job.scales, &RESOLUTIONS);
    }

    #[test]
    fn test_convert_job_deserialize_with_custom_scales() {
        // Test deserializing new JSON format with custom scales
        let json = r#"{
            "id": "test-job-2",
            "crf": 30,
            "scales": [{"w": 1920, "h": 1080}, {"w": 1280, "h": 720}]
        }"#;

        let job: ConvertJob = serde_json::from_str(json).unwrap();

        assert_eq!(job.id, "test-job-2");
        assert_eq!(job.crf, 30);
        assert_eq!(job.scales.len(), 2);
        assert_eq!(job.scales.first().unwrap().width(), 1920);
        assert_eq!(job.scales.first().unwrap().height(), 1080);
        assert_eq!(job.scales.get(1).unwrap().width(), 1280);
        assert_eq!(job.scales.get(1).unwrap().height(), 720);
    }

    #[test]
    fn test_convert_job_deserialize_with_empty_scales() {
        // Test deserializing JSON with empty scales array - should fallback to defaults
        let json = r#"{
            "id": "test-job-3",
            "crf": 25,
            "scales": []
        }"#;

        let job: ConvertJob = serde_json::from_str(json).unwrap();

        assert_eq!(job.id, "test-job-3");
        assert_eq!(job.crf, 25);
        // Empty scales array in JSON should deref to default RESOLUTIONS
        assert_eq!(job.scales.len(), RESOLUTIONS.len());
        assert!(!job.scales.is_empty()); // Deref makes it non-empty
        assert_eq!(&*job.scales, &RESOLUTIONS);
    }

    #[test]
    fn test_convert_job_serialize_with_default_scales() {
        // Test serializing job with default scales (should skip the field)
        let job = ConvertJob {
            id: "test-job-4".to_string(),
            crf: 20,
            scales: Scales::new(),
            retry_times: Arc::new(0.into()),
        };

        let json = serde_json::to_string(&job).unwrap();

        // Should not contain scales field when using defaults
        assert!(!json.contains("scales"));
        assert!(json.contains(r#""id":"test-job-4""#));
        assert!(json.contains(r#""crf":20"#));
    }

    #[test]
    fn test_convert_job_serialize_with_empty_scales() {
        // Test serializing job with empty scales (should skip the field)
        let job = ConvertJob {
            id: "test-job-5".to_string(),
            crf: 18,
            scales: Scales::from_vec(Vec::new()),
            retry_times: Arc::new(0.into()),
        };

        let json = serde_json::to_string(&job).unwrap();

        // Should not contain scales field when empty
        assert!(!json.contains("scales"));
        assert!(json.contains(r#""id":"test-job-5""#));
        assert!(json.contains(r#""crf":18"#));
    }

    #[test]
    fn test_convert_job_serialize_with_custom_scales() {
        // Test serializing job with custom scales (should include the field)
        let custom_scales = vec![
            Scale::new(1920, 1080),
            Scale::new(1280, 720),
            Scale::new(854, 480),
        ];
        let job = ConvertJob {
            id: "test-job-6".to_string(),
            crf: 22,
            scales: Scales::from_vec(custom_scales),
            retry_times: Arc::new(0.into()),
        };

        let json = serde_json::to_string(&job).unwrap();

        // Should contain scales field when using custom values
        assert!(json.contains("scales"));
        assert!(json.contains(r#""id":"test-job-6""#));
        assert!(json.contains(r#""crf":22"#));
        assert!(json.contains(r#""w":1920"#));
        assert!(json.contains(r#""h":1080"#));
        assert!(json.contains(r#""w":1280"#));
        assert!(json.contains(r#""h":720"#));
    }

    #[test]
    fn test_convert_job_round_trip_legacy() {
        // Test round trip: serialize -> deserialize for legacy format (default scales)
        let original_job = ConvertJob {
            id: "round-trip-1".to_string(),
            crf: 24,
            scales: Scales::new(),
            retry_times: Arc::new(0.into()),
        };

        let json = serde_json::to_string(&original_job).unwrap();
        let deserialized_job: ConvertJob = serde_json::from_str(&json).unwrap();

        assert_eq!(original_job.id, deserialized_job.id);
        assert_eq!(original_job.crf, deserialized_job.crf);
        // Both should use default scales
        assert_eq!(&*original_job.scales, &RESOLUTIONS);
        assert_eq!(&*deserialized_job.scales, &RESOLUTIONS);
    }

    #[test]
    fn test_convert_job_round_trip_custom() {
        // Test round trip: serialize -> deserialize for custom scales
        let custom_scales = vec![Scale::new(1920, 1080), Scale::new(1280, 720)];
        let original_job = ConvertJob {
            id: "round-trip-2".to_string(),
            crf: 26,
            scales: Scales::from_vec(custom_scales.clone()),
            retry_times: Arc::new(0.into()),
        };

        let json = serde_json::to_string(&original_job).unwrap();
        let deserialized_job: ConvertJob = serde_json::from_str(&json).unwrap();

        assert_eq!(original_job.id, deserialized_job.id);
        assert_eq!(original_job.crf, deserialized_job.crf);
        assert_eq!(original_job.scales.len(), deserialized_job.scales.len());

        // Verify the actual scale values
        assert_eq!(deserialized_job.scales.len(), 2);
        assert_eq!(deserialized_job.scales.first().unwrap().width(), 1920);
        assert_eq!(deserialized_job.scales.first().unwrap().height(), 1080);
        assert_eq!(deserialized_job.scales.get(1).unwrap().width(), 1280);
        assert_eq!(deserialized_job.scales.get(1).unwrap().height(), 720);
    }

    #[test]
    fn test_convert_job_constructor() {
        let scales = Scales::from_vec(vec![Scale::new(1920, 1080)]);
        let job = ConvertJob::new("constructor-test".to_string(), 28, scales.clone());

        assert_eq!(job.id(), "constructor-test");
        assert_eq!(job.crf(), 28);
        assert_eq!(job.scales.len(), scales.len());
    }

    #[test]
    fn test_forward_compatibility_scenarios() {
        // Test various backward compatibility scenarios

        // 1. Old job without scales field should work
        let old_json = r#"{"id":"old-job","crf":23}"#;
        let old_job: ConvertJob = serde_json::from_str(old_json).unwrap();
        assert_eq!(&*old_job.scales, &RESOLUTIONS);

        // 2. Job with null scales should work (fallback to default)
        let null_json = r#"{"id":"null-job","crf":25,"scales":null}"#;
        let _null_job_result: Result<ConvertJob, _> = serde_json::from_str(null_json);
        // This should fail gracefully or use defaults depending on serde behavior

        // 3. Job with different scale format should handle gracefully
        let minimal_json = r#"{"id":"minimal","crf":20,"scales":[]}"#;
        let minimal_job: ConvertJob = serde_json::from_str(minimal_json).unwrap();
        assert_eq!(&*minimal_job.scales, &RESOLUTIONS); // Empty scales should deref to default
    }
}
