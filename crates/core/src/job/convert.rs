use crate::app_state::AppState;
use crate::job::upload::UploadJob;
use crate::job::{Action, CONVERT_KIND, FailureJob, Job, JobKind, RawJob};
use anyhow::{Result, anyhow};
use ez_ffmpeg::Frame;
use ez_ffmpeg::container_info::get_duration_us;
use ez_ffmpeg::filter::frame_filter::FrameFilter;
use ez_ffmpeg::filter::frame_filter_context::FrameFilterContext;
use ez_ffmpeg::filter::frame_pipeline_builder::FramePipelineBuilder;
use ez_ffmpeg::{AVMediaType, Input};
use ez_ffmpeg::{FfmpegContext, Output};
use ffmpeg_next::{StreamMut, codec, format, media};
use rayon::ThreadPool;
use rayon::ThreadPoolBuilder;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::oneshot;
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
            w if w >= 720 => 735_000,
            w if w >= 540 => 495_000,
            _ => 430_000,
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

#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConvertCodec {
    Vp9,
    H265,
    H264,
}

pub const DEFAULT_CODECS: [ConvertCodec; 2] = [ConvertCodec::Vp9, ConvertCodec::H264];

impl ConvertCodec {
    fn playlist_suffix(self) -> &'static str {
        match self {
            ConvertCodec::Vp9 => "",
            ConvertCodec::H265 => "-h265",
            ConvertCodec::H264 => "-h264",
        }
    }

    fn playlist_filename(self, job_id: &str) -> String {
        format!("{job_id}{}.m3u8", self.playlist_suffix())
    }

    fn segment_prefix(self, job_id: &str) -> String {
        match self {
            ConvertCodec::Vp9 => job_id.to_string(),
            ConvertCodec::H265 => format!("{job_id}-h265"),
            ConvertCodec::H264 => format!("{job_id}-h264"),
        }
    }

    fn init_filename(self, job_id: &str) -> String {
        format!("{}-init.mp4", self.segment_prefix(job_id))
    }

    fn audio_encoder(self) -> &'static str {
        match self {
            ConvertCodec::Vp9 => "libopus",
            ConvertCodec::H265 | ConvertCodec::H264 => "aac",
        }
    }

    fn video_encoder(self) -> &'static str {
        match self {
            ConvertCodec::Vp9 => "libvpx-vp9",
            ConvertCodec::H265 => "libx265",
            ConvertCodec::H264 => "libx264",
        }
    }

    fn codecs_attribute(self) -> &'static str {
        match self {
            ConvertCodec::Vp9 => "vp09.00.51.08.01.01.01.01.00,opus",
            ConvertCodec::H265 => "hvc1.1.6.L90.B0,mp4a.40.2",
            ConvertCodec::H264 => "avc1.4D401F,mp4a.40.2",
        }
    }

    fn apply_codec_settings(self, output: Output) -> Output {
        if let ConvertCodec::H265 = self {
            output
                .set_video_codec_opt("g", "120")
                .set_video_codec_opt("keyint_min", "120")
                .set_video_codec_opt("sc_threshold", "0")
        } else {
            output
        }
        .set_audio_codec(self.audio_encoder())
        .set_video_codec(self.video_encoder())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct ConvertCodecs(Vec<ConvertCodec>);

impl ConvertCodecs {
    pub fn new() -> Self {
        Self(DEFAULT_CODECS.to_vec())
    }

    pub fn from_vec(codecs: Vec<ConvertCodec>) -> Self {
        Self(codecs)
    }

    pub fn skip_serialize(&self) -> bool {
        self.0.is_empty() || self.0.as_slice() == DEFAULT_CODECS
    }
}

impl Deref for ConvertCodecs {
    type Target = [ConvertCodec];

    fn deref(&self) -> &Self::Target {
        if self.0.is_empty() {
            &DEFAULT_CODECS
        } else {
            &self.0[..]
        }
    }
}

impl Default for ConvertCodecs {
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
    #[serde(skip_serializing_if = "ConvertCodecs::skip_serialize")]
    pub codecs: ConvertCodecs,

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
            codecs: ConvertCodecs::default(),
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

    fn need_permit(&self) -> usize {
        self.codecs.len().saturating_mul(self.scales.len())
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn gen_job(&self, state: AppState) -> TokioJoinHandle<anyhow::Result<()>> {
        let job = self.clone();
        tokio::spawn(async move {
            let temp_dir = state.temp_dir().join(format!("tmp-{}", job.id()));
            let sanitized_input = format!("/tmp/{}-sanitized.mp4", job.id());
            let out_dir = state.videos_dir().join(job.id());

            // Remove existing directory
            _ = std::fs::remove_file(&sanitized_input);
            _ = std::fs::remove_dir_all(&temp_dir);
            _ = std::fs::remove_dir_all(&out_dir);

            _ = std::fs::File::create(&sanitized_input)?;
            tokio::fs::create_dir_all(&temp_dir).await?;

            let job_id = job.id().to_string();
            let webhook_state = state.clone();
            spawn_convert_job(job, state, temp_dir, sanitized_input, out_dir)
                .await
                .map_err(|_| anyhow!("convert worker dropped"))??;

            webhook_state
                .call_webhook(&job_id, CONVERT_KIND, "completed")
                .await;

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

fn spawn_convert_job(
    job: ConvertJob,
    state: AppState,
    temp_dir: PathBuf,
    sanitized_input: String,
    out_dir: PathBuf,
) -> oneshot::Receiver<anyhow::Result<()>> {
    let (tx, rx) = oneshot::channel();
    convert_pool().spawn_fifo(move || {
        let result = run_blocking_convert(job, state, temp_dir, sanitized_input, out_dir);
        let _ = tx.send(result);
    });

    rx
}

fn run_blocking_convert(
    job: ConvertJob,
    state: AppState,
    temp_dir: PathBuf,
    sanitized_input: String,
    out_dir: PathBuf,
) -> anyhow::Result<()> {
    let input_path = job.upload_path(&state);

    if let Err(error) = remux_av_only(input_path.to_str().unwrap(), &sanitized_input) {
        warn!(?error, "Remux failed, using original file for conversion");
        _ = std::fs::remove_file(&sanitized_input);
        std::fs::copy(&input_path, &sanitized_input)?;
    };

    create_master_playlist(&job, &sanitized_input, &temp_dir)?;

    _ = std::fs::remove_file(&sanitized_input);
    std::fs::rename(&temp_dir, &out_dir)?;
    // Cleanup original file
    _ = std::fs::remove_file(&input_path);
    info!(job_id = job.id(), "HLS conversion completed successfully");

    Ok(())
}

fn convert_pool() -> &'static ThreadPool {
    static CONVERT_POOL: OnceLock<ThreadPool> = OnceLock::new();
    CONVERT_POOL.get_or_init(|| {
        ThreadPoolBuilder::new()
            .thread_name(|index| format!("convert-{index}"))
            .build()
            .expect("create convert worker pool")
    })
}

/// Convert MP4 to requested codec variants (VP9, H265).
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
        codecs,
        ..
    } = job;

    // Create outputs for each codec+resolution combination
    let mut outputs = Vec::new();

    for (scale_idx, scale) in scales.iter().enumerate() {
        let w = scale.width();
        let out_dir = output.join(w.to_string());
        std::fs::create_dir_all(&out_dir)?;

        for codec in codecs.iter() {
            let frame_pipeline: FramePipelineBuilder = AVMediaType::AVMEDIA_TYPE_AUDIO.into();
            let progress_filter = ProgressCallBackFilter::new(progress_callbacker.clone());
            let pipe = frame_pipeline.filter("progress", Box::new(progress_filter));

            let playlist_path = out_dir.join(codec.playlist_filename(job_id));
            let output_str = out_dir.to_str().unwrap();
            let hls_segment_filename =
                format!("{output_str}/{}-%03d.m4s", codec.segment_prefix(job_id));

            let mut output_builder = Output::from(playlist_path.to_str().unwrap())
                .add_frame_pipeline(pipe)
                .add_stream_map("0:a?")
                .add_stream_map(format!("v{scale_idx}{}", codec.segment_prefix("")));
            output_builder = codec
                .apply_codec_settings(output_builder)
                .set_video_codec_opt("crf", crf.to_string())
                .set_format("hls")
                .set_format_opt("hls_time", HLS_SEGMENT_DURATION.to_string())
                .set_format_opt("hls_segment_type", "fmp4")
                .set_format_opt("hls_playlist_type", "vod")
                .set_format_opt("hls_fmp4_init_filename", codec.init_filename(job_id))
                .set_format_opt("hls_segment_filename", hls_segment_filename);

            outputs.push(output_builder);
        }
    }

    let mut filter_parts = Vec::new();

    // Create scale filters for each resolution
    let target_scales = scales.len();
    let scale_outputs: String = (0..target_scales).map(|i| format!("[scaled{i}]")).collect();
    filter_parts.push(format!("[0:v]split={target_scales}{scale_outputs}"));

    for (scale_idx, scale) in scales.iter().enumerate() {
        let (w, h) = scale.into();
        filter_parts.push(format!(
            "[scaled{scale_idx}]scale={w}:{h}[v{scale_idx}_base]"
        ));
    }

    // Create codec-specific streams for each resolution
    for (scale_idx, _scale) in scales.iter().enumerate() {
        let len = codecs.len();
        let codec_outputs: String = codecs
            .iter()
            .map(|codec| format!("[v{scale_idx}{}]", codec.segment_prefix("")))
            .collect();
        filter_parts.push(format!("[v{scale_idx}_base]split={len}{codec_outputs}"));
    }

    let graphs = filter_parts.join(";");
    debug!(%job_id, %crf, ?input, ?graphs, "Converting to HLS");

    let input = Input::from(input);
    let builder = FfmpegContext::builder().input(input).filter_desc(graphs);
    Ok(builder.outputs(outputs).build()?.start()?.wait()?)
}

/// Clear codec tag from stream
fn clear_codec_tag(stream: &mut StreamMut) {
    unsafe {
        let av_stream = stream.as_mut_ptr();
        if !av_stream.is_null() && !(*av_stream).codecpar.is_null() {
            (*(*av_stream).codecpar).codec_tag = 0;
        }
    }
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
                output_stream.set_time_base(stream.time_base());
                output_stream.set_parameters(stream.parameters());

                clear_codec_tag(&mut output_stream);

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
    debug!(%job_id, "Duration: {total} us");

    let mut progress_callbacker = ProgressCallBacker::new(job_id.to_string());
    progress_callbacker.total_duration = total;

    // Retrieve the audio stream information
    let audio_info = ez_ffmpeg::stream_info::find_audio_stream_info(input).unwrap();
    if let Some(audio_info) = audio_info {
        if let ez_ffmpeg::stream_info::StreamInfo::Audio { time_base, .. } = audio_info {
            progress_callbacker.time_base = time_base;
            debug!(%job_id, "Audio time base: {}/{}", time_base.num, time_base.den);
        }
    } else {
        warn!("Audio stream information not found");
    }

    convert(job, input, output, progress_callbacker.into())
        .inspect_err(|error| error!(?error, %job_id, "Failed to convert to HLS with scales"))?;
    // FIXME: h265 in apple is not supported yet

    let master_playlist_path = output.join(format!("{job_id}.m3u8"));
    let mut file = std::fs::File::create(&master_playlist_path)?;

    let mut content = String::new();
    content.push_str("#EXTM3U\n");
    content.push_str("#EXT-X-VERSION:7\n");
    content.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");

    // #EXTM3U
    // #EXT-X-VERSION:3
    // #EXT-X-INDEPENDENT-SEGMENTS
    // Generate entries for each scale in the job
    for codec in job.codecs.iter() {
        for (w, h) in job.scales.iter().map(From::from) {
            let bandwidth = Scales::bandwidth(w);
            let codecs_attr = codec.codecs_attribute();
            content.push_str(&format!(
                "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth},RESOLUTION={w}x{h},CODECS=\"{codecs_attr}\"\n"
            ));
            content.push_str(&format!("{w}/{}\n", codec.playlist_filename(job_id)));
        }
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
            codecs: ConvertCodecs::default(),
            retry_times: Arc::new(0.into()),
        };

        let json = serde_json::to_string(&job).unwrap();

        // Should not contain scales field when using defaults
        assert!(!json.contains("scales"));
        assert!(!json.contains("codecs"));
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
            codecs: ConvertCodecs::default(),
            retry_times: Arc::new(0.into()),
        };

        let json = serde_json::to_string(&job).unwrap();

        // Should not contain scales field when empty
        assert!(!json.contains("scales"));
        assert!(!json.contains("codecs"));
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
            codecs: ConvertCodecs::default(),
            retry_times: Arc::new(0.into()),
        };

        let json = serde_json::to_string(&job).unwrap();

        // Should contain scales field when using custom values
        assert!(json.contains("scales"));
        assert!(!json.contains("codecs"));
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
            codecs: ConvertCodecs::default(),
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
            codecs: ConvertCodecs::default(),
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
        assert_eq!(&*job.codecs, &DEFAULT_CODECS);
    }

    #[test]
    fn test_forward_compatibility_scenarios() {
        // Test various backward compatibility scenarios

        // 1. Old job without scales field should work
        let old_json = r#"{"id":"old-job","crf":23}"#;
        let old_job: ConvertJob = serde_json::from_str(old_json).unwrap();
        assert_eq!(&*old_job.scales, &RESOLUTIONS);
        assert_eq!(&*old_job.codecs, &DEFAULT_CODECS);

        // 2. Job with null scales should work (fallback to default)
        let null_json = r#"{"id":"null-job","crf":25,"scales":null}"#;
        let _null_job_result: Result<ConvertJob, _> = serde_json::from_str(null_json);
        // This should fail gracefully or use defaults depending on serde behavior

        // 3. Job with different scale format should handle gracefully
        let minimal_json = r#"{"id":"minimal","crf":20,"scales":[]}"#;
        let minimal_job: ConvertJob = serde_json::from_str(minimal_json).unwrap();
        assert_eq!(&*minimal_job.scales, &RESOLUTIONS); // Empty scales should deref to default
        assert_eq!(&*minimal_job.codecs, &DEFAULT_CODECS);
    }

    #[test]
    fn test_convert_job_only_h265_serialization() {
        let mut job = ConvertJob::new("h265-only".to_string(), 30, Scales::new());
        job.codecs = ConvertCodecs::from_vec(vec![ConvertCodec::H265]);

        let json = serde_json::to_string(&job).unwrap();
        assert!(json.contains("codecs"));
        assert!(json.contains("h265"));

        let parsed: ConvertJob = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.codecs.len(), 1);
        assert_eq!(parsed.codecs[0], ConvertCodec::H265);
    }

    #[test]
    fn test_convert_job_deserialize_with_codecs() {
        let json = r#"{"id":"custom","crf":28,"codecs":["h265","vp9"]}"#;
        let job: ConvertJob = serde_json::from_str(json).unwrap();

        assert_eq!(job.codecs.len(), 2);
        assert_eq!(job.codecs[0], ConvertCodec::H265);
        assert_eq!(job.codecs[1], ConvertCodec::Vp9);
    }
}
