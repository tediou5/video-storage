use crate::job::JobGenerator;
use crate::{BANDWIDTHS, Job, RESOLUTIONS};

use std::io::Write as _;
use std::path::Path;
#[cfg(feature = "cuda")]
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use ez_ffmpeg::AVMediaType;
use ez_ffmpeg::Frame;
use ez_ffmpeg::container_info::get_duration_us;
use ez_ffmpeg::filter::frame_filter::FrameFilter;
use ez_ffmpeg::filter::frame_filter_context::FrameFilterContext;
use ez_ffmpeg::filter::frame_pipeline_builder::FramePipelineBuilder;
use ez_ffmpeg::{FfmpegContext, Input, Output};
use tracing::{error, info, warn};

/// Global GPU availability flag (only used when feature "cuda" is enabled).
/// 0 = unknown, 1 = allowed, 2 = disabled (do not try GPU again in this process)
#[cfg(feature = "cuda")]
static GPU_TRY_STATE: AtomicU8 = AtomicU8::new(0);

pub(crate) async fn spawn_hls_job(
    JobGenerator { job, upload_path }: JobGenerator,
    videos_dir: &Path,
    temp_dir: &Path,
) -> anyhow::Result<()> {
    let temp_dir = temp_dir.join(format!("tmp-{}", job.id()));
    let out_dir = videos_dir.join(job.id());
    tokio::fs::create_dir_all(&temp_dir).await?;

    let input_path = upload_path.clone();
    tokio::task::spawn_blocking(move || {
        create_master_playlist(&job, &input_path, &temp_dir)?;
        // Remove existing directory if it exists
        _ = std::fs::remove_dir_all(&out_dir);
        std::fs::rename(&temp_dir, &out_dir)?;
        info!(
            job_id = job.id(),
            output_dir = ?out_dir.display(),
            "HLS conversion completed successfully."
        );
        Ok::<(), anyhow::Error>(())
    })
    .await??;

    Ok(())
}

#[inline]
fn vp9_tile_params(width: u32, height: u32) -> (u8, u8) {
    fn floor_log2(mut x: u32) -> u8 {
        let mut l = 0;
        while x >= 2 {
            x >>= 1;
            l += 1;
        }
        l
    }
    let cols = if width < 64 {
        0
    } else {
        floor_log2(width / 64).min(6)
    };
    let rows = if height < 64 {
        0
    } else {
        floor_log2(height / 64).min(2)
    };
    (cols, rows)
}

// Build per-variant Output list fresh each time (Input/Output are not Clone).
fn build_outputs(
    job_id: &str,
    crf: u8,
    output_root: &Path,
    scales: &[(u32, u32)],
    progress_callbacker: &Arc<ProgressCallBacker>,
) -> anyhow::Result<Vec<Output>> {
    let mut outs = Vec::with_capacity(scales.len());
    for (i, &(w, h)) in scales.iter().enumerate() {
        // Progress callback pipeline (kept from your original code)
        let frame_pipeline: FramePipelineBuilder = AVMediaType::AVMEDIA_TYPE_AUDIO.into();
        let progress_filter = ProgressCallBackFilter::new(progress_callbacker.clone());
        let pipe = frame_pipeline.filter("progress", Box::new(progress_filter));

        // Ensure output dir exists
        let out_dir = output_root.join(w.to_string());
        std::fs::create_dir_all(&out_dir)?;

        // HLS paths
        let playlist_path = out_dir.join(format!("{job_id}.m3u8"));
        let output_str = out_dir.to_str().expect("utf8 path");
        let hls_segment_filename = format!("{output_str}/{job_id}-%03d.m4s");

        // VP9 VOD preset (CQ + parallelism)
        let (tc, tr) = vp9_tile_params(w, h);

        let output = Output::from(playlist_path.to_str().unwrap())
            .add_frame_pipeline(pipe)
            // Video: libvpx-vp9 VOD settings
            .set_video_codec("libvpx-vp9")
            .set_video_codec_opt("b", "0") // CQ mode
            .set_video_codec_opt("crf", crf.to_string())
            .set_video_codec_opt("row-mt", "1")
            .set_video_codec_opt("tile-columns", tc.to_string())
            .set_video_codec_opt("tile-rows", tr.to_string())
            .set_video_codec_opt("cpu-used", "2") // 1 = slower/better; 2 = balanced
            .set_video_codec_opt("lag-in-frames", "25")
            .set_video_codec_opt("auto-alt-ref", "1")
            // .set_video_codec_opt("g", "240")                   // optional GOP if needed
            // Audio: Opus
            .set_audio_codec("libopus")
            .set_audio_codec_opt("b", "128k")
            // Stream mapping: current output expects v{i} + optional 0:a?
            .add_stream_map("0:a?")
            .add_stream_map(format!("v{i}"))
            // HLS (fMP4) VOD settings
            .set_format("hls")
            .set_format_opt("hls_time", "4")
            .set_format_opt("hls_segment_type", "fmp4")
            .set_format_opt("hls_playlist_type", "vod")
            .set_format_opt("hls_list_size", "0")
            .set_format_opt("hls_fmp4_init_filename", format!("{job_id}-init.mp4"))
            .set_format_opt("hls_segment_filename", hls_segment_filename);

        outs.push(output);
    }
    Ok(outs)
}

// Build a CPU-only filter graph: split → per-branch scale → yuv420p
fn graph_cpu_only(scales: &[(u32, u32)]) -> String {
    let n = scales.len();
    let (outs, chains): (String, String) = scales
        .iter()
        .enumerate()
        .map(|(i, &(w, h))| {
            (
                format!("[out{i}]"),
                format!(";[out{i}]scale={w}:{h},format=yuv420p[v{i}]"),
            )
        })
        .unzip();
    format!("[0:v]split={n}{outs}{chains}")
}

// Build a GPU scaling graph with the given filter name ("scale_npp" or "scale_cuda").
// We keep frames on GPU in NV12, then hwdownload → yuv420p for CPU-side VP9 encoder.
#[cfg(feature = "cuda")]
fn graph_gpu_scale(scales: &[(u32, u32)], filter_name: &str) -> String {
    let n = scales.len();
    let (outs, chains): (String, String) = scales
        .iter()
        .enumerate()
        .map(|(i, &(w, h))| {
            (
                format!("[out{i}]"),
                format!(
                    // named options are mandatory for these filters
                    ";[out{i}]{filter}=w={w}:h={h}:format=nv12,hwdownload,format=yuv420p[v{i}]",
                    filter = filter_name
                ),
            )
        })
        .unzip();
    format!("[0:v]split={n}{outs}{chains}")
}

// Run a single ffmpeg pipeline attempt.
fn run_once(
    use_cuda: bool,
    in_path: &str,
    graph: &str,
    outputs: Vec<Output>,
) -> anyhow::Result<()> {
    let mut in0 = Input::from(in_path);
    if use_cuda {
        // Enable NVDEC; frames come out as HW surfaces (cuda)
        in0 = in0.set_hwaccel("cuda").set_hwaccel_output_format("cuda");
    }
    FfmpegContext::builder()
        .input(in0)
        .filter_desc(graph.to_string())
        .outputs(outputs)
        .build()?
        .start()?
        .wait()?;
    Ok(())
}

// ---- Main entry ----

/// Convert MP4 → multi-scale HLS (fMP4) with VP9+Opus (VOD).
/// CPU-only by default. When built with feature "cuda", it attempts GPU scaling:
///   1) scale_npp, then 2) scale_cuda, and if both fail, permanently disables GPU attempts
///   for this process and falls back to pure CPU (to avoid noisy logs across many jobs).
pub fn convert_with_scales(
    job: &Job,
    input: &Path,
    output_root: &Path,
    scales: &[(u32, u32)],
    progress_callbacker: Arc<ProgressCallBacker>,
) -> anyhow::Result<()> {
    let Job { id: job_id, crf } = job;
    let in_path = input.to_str().expect("utf8 path");

    // CPU-only path (default build)
    #[cfg(not(feature = "cuda"))]
    {
        let graph = graph_cpu_only(scales);
        let outs = build_outputs(job_id, *crf, output_root, scales, &progress_callbacker)?;
        run_once(false, in_path, &graph, outs)?;
        return Ok(());
    }

    // CUDA build: try GPU once unless globally disabled; otherwise go CPU.
    #[cfg(feature = "cuda")]
    {
        // If we already marked GPU as disabled, go straight to CPU.
        if GPU_TRY_STATE.load(Ordering::Relaxed) == 2 {
            let graph = graph_cpu_only(scales);
            let outs = build_outputs(job_id, *crf, output_root, scales, &progress_callbacker)?;
            run_once(false, in_path, &graph, outs)?;
            return Ok(());
        }

        // First time: mark as "allowed" and try GPU filters in order.
        GPU_TRY_STATE
            .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
            .ok();

        // Try scale_npp
        let graph_npp = graph_gpu_scale(scales, "scale_npp");
        let outs_npp = build_outputs(job_id, *crf, output_root, scales, &progress_callbacker)?;
        match run_once(true, in_path, &graph_npp, outs_npp) {
            Ok(()) => return Ok(()),
            Err(e_npp) => {
                error!("GPU scale_npp failed (will try scale_cuda): {e_npp:?}");
            }
        }

        // Try scale_cuda
        let graph_cuda = graph_gpu_scale(scales, "scale_cuda");
        let outs_cuda = build_outputs(job_id, *crf, output_root, scales, &progress_callbacker)?;
        match run_once(true, in_path, &graph_cuda, outs_cuda) {
            Ok(()) => return Ok(()),
            Err(e_cuda) => {
                error!("GPU scale_cuda failed (disabling GPU and falling back to CPU): {e_cuda:?}");
                GPU_TRY_STATE.store(2, Ordering::Relaxed); // permanently disable GPU path
            }
        }

        // Pure CPU fallback (disable hwaccel and use CPU graph without hwdownload).
        let graph_cpu = graph_cpu_only(scales);
        let outs_cpu = build_outputs(job_id, *crf, output_root, scales, &progress_callbacker)?;
        run_once(false, in_path, &graph_cpu, outs_cpu)?;
        return Ok(());
    }
}

/// Convert MP4 → multi-scale HLS(fMP4) VP9+Opus (VOD preset)
pub fn _convert_with_scales(
    job: &Job,
    input: &Path,
    output: &Path,
    scales: &[(u32, u32)],
    progress_callbacker: Arc<ProgressCallBacker>,
) -> anyhow::Result<()> {
    use ez_ffmpeg::{FfmpegContext, Input, Output};

    let Job { id: job_id, crf } = job;
    let in_path = input.to_str().expect("utf8 path");

    // Build outputs fresh for each attempt (no Clone on Output).
    let build_outputs = || -> anyhow::Result<Vec<Output>> {
        scales
            .iter()
            .copied()
            .enumerate()
            .map(|(i, (w, h))| {
                // Progress callback pipeline (as in your original code).
                let frame_pipeline: FramePipelineBuilder = AVMediaType::AVMEDIA_TYPE_AUDIO.into();
                let progress_filter = ProgressCallBackFilter::new(progress_callbacker.clone());
                let pipe = frame_pipeline.filter("progress", Box::new(progress_filter));

                // Prepare directories and playlist/segment names.
                let out_dir = output.join(w.to_string());
                std::fs::create_dir_all(&out_dir)?;
                let playlist_path = out_dir.join(format!("{job_id}.m3u8"));
                let output_str = out_dir.to_str().expect("utf8 path");
                let hls_segment_filename = format!("{output_str}/{job_id}-%03d.m4s");

                // VP9 VOD preset (CQ + parallelism).
                let (tc, tr) = vp9_tile_params(w, h);

                Ok(Output::from(playlist_path.to_str().unwrap())
                    .add_frame_pipeline(pipe)
                    // Video: libvpx-vp9 VOD settings
                    .set_video_codec("libvpx-vp9")
                    .set_video_codec_opt("b", "0") // CQ mode
                    .set_video_codec_opt("crf", crf.to_string()) // your job.crf
                    .set_video_codec_opt("row-mt", "1")
                    .set_video_codec_opt("tile-columns", tc.to_string())
                    .set_video_codec_opt("tile-rows", tr.to_string())
                    .set_video_codec_opt("cpu-used", "2") // 1 = slower/better; 2 = balanced
                    .set_video_codec_opt("lag-in-frames", "25")
                    .set_video_codec_opt("auto-alt-ref", "1")
                    // .set_video_codec_opt("g", "240")                   // optional GOP if needed
                    // Audio: Opus
                    .set_audio_codec("libopus")
                    .set_audio_codec_opt("b", "128k")
                    // Stream mapping: current output expects v{i} + optional 0:a?
                    .add_stream_map("0:a?")
                    .add_stream_map(format!("v{i}"))
                    // HLS (fMP4) VOD settings
                    .set_format("hls")
                    .set_format_opt("hls_time", "4")
                    .set_format_opt("hls_segment_type", "fmp4")
                    .set_format_opt("hls_playlist_type", "vod")
                    .set_format_opt("hls_list_size", "0")
                    .set_format_opt("hls_fmp4_init_filename", format!("{job_id}-init.mp4"))
                    .set_format_opt("hls_segment_filename", hls_segment_filename))
            })
            .collect()
    };

    // Build filter graphs.
    let n = scales.len();

    // CPU-only graph: simple split → per-branch scale → yuv420p.
    let (outs_cpu, branches_cpu): (String, String) = scales
        .iter()
        .enumerate()
        .map(|(i, (w, h))| {
            (
                format!("[out{i}]"),
                format!(";[out{i}]scale={w}:{h},format=yuv420p[v{i}]"),
            )
        })
        .unzip();
    let graph_cpu_pure = format!("[0:v]split={n}{outs_cpu}{branches_cpu}");

    // Runner helper: build Input and run once with a given graph; toggle hwaccel if needed.
    let run_once = |use_cuda: bool, graph: &str| -> anyhow::Result<()> {
        let mut in0 = Input::from(in_path);
        if use_cuda {
            in0 = in0.set_hwaccel("cuda").set_hwaccel_output_format("cuda");
        }
        let outputs = build_outputs()?;
        FfmpegContext::builder()
            .input(in0)
            .filter_desc(graph.to_string())
            .outputs(outputs)
            .build()?
            .start()?
            .wait()?;
        Ok(())
    };

    // Default build (no "cuda" feature): pure CPU path.
    #[cfg(not(feature = "cuda"))]
    {
        // debug!(job_id=%job_id, crf=%crf, graph=%graph_cpu_pure, "HLS VP9 (CPU)");
        run_once(false, &graph_cpu_pure)?;
        return Ok(());
    }

    // "cuda" feature build: try GPU scaling first, then fall back to CPU.
    #[cfg(feature = "cuda")]
    {
        // GPU path #1: scale_npp (named options; produce NV12 on GPU, then hwdownload → yuv420p).
        let (outs_npp, branches_npp): (String, String) = scales
            .iter()
            .enumerate()
            .map(|(i, (w, h))| {
                (
                    format!("[out{i}]"),
                    format!(
                        ";[out{i}]scale_npp=w={w}:h={h}:format=nv12,hwdownload,format=yuv420p[v{i}]"
                    ),
                )
            })
            .unzip();
        let graph_gpu_npp = format!("[0:v]split={n}{outs_npp}{branches_npp}");

        // GPU path #2: scale_cuda (fallback if NPP is not available).
        let (outs_cuda, branches_cuda): (String, String) = scales.iter().enumerate()
                .map(|(i, (w, h))| {
                    (format!("[out{i}]"),
                     format!(";[out{i}]scale_cuda=w={w}:h={h}:format=nv12,hwdownload,format=yuv420p[v{i}]"))
                })
                .unzip();
        let graph_gpu_cuda = format!("[0:v]split={n}{outs_cuda}{branches_cuda}");

        // Final fallback: pure CPU (disable hwaccel; no hwdownload in graph).
        // debug!(job_id=%job_id, crf=%crf, graph=%graph_gpu_npp, "HLS VP9 (CUDA try NPP)");
        if let Err(e1) = run_once(true, &graph_gpu_npp) {
            error!("GPU scale_npp failed: {e1:?}");
            // debug!(job_id=%job_id, crf=%crf, graph=%graph_gpu_cuda, "HLS VP9 (CUDA try scale_cuda)");
            if let Err(e2) = run_once(true, &graph_gpu_cuda) {
                error!("GPU scale_cuda failed: {e2:?}, falling back to pure CPU...");
                // debug!(job_id=%job_id, crf=%crf, graph=%graph_cpu_pure, "HLS VP9 (CPU fallback)");
                run_once(false, &graph_cpu_pure)?;
            }
        }
        return Ok(());
    }
}

pub(crate) fn create_master_playlist(job: &Job, input: &Path, output: &Path) -> anyhow::Result<()> {
    let job_id = job.id();

    let input_str = input.to_str().unwrap();
    let total = get_duration_us(input_str).unwrap();

    let mut progress_callbacker = ProgressCallBacker::new(job_id.to_string());
    progress_callbacker.total_duration = total;

    // Retrieve the audio stream information
    let audio_info = ez_ffmpeg::stream_info::find_audio_stream_info(input_str).unwrap();
    if let Some(audio_info) = audio_info {
        if let ez_ffmpeg::stream_info::StreamInfo::Audio { time_base, .. } = audio_info {
            progress_callbacker.time_base = time_base;
            info!(%job_id, "Audio time base: {}/{}", time_base.num, time_base.den);
        }
    } else {
        warn!("Audio stream information not found");
    }

    convert_with_scales(job, input, output, &RESOLUTIONS, progress_callbacker.into())
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
    // #EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720
    // 720/{job-id}.m3u8
    // #EXT-X-STREAM-INF:BANDWIDTH=1500000,RESOLUTION=960x540
    // 540/{job-id}.m3u8
    // #EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=854x480
    // 480/{job-id}.m3u8
    for ((width, height), bandwidth) in RESOLUTIONS.iter().zip(BANDWIDTHS.iter()) {
        content.push_str(&format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth},RESOLUTION={width}x{height},CODECS=\"vp09.00.51.08.01.01.01.01.00,opus\"\n"
        ));
        content.push_str(&format!("{width}/{job_id}.m3u8\n"));
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
