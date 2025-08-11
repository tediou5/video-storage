use crate::job::JobGenerator;
use crate::{BANDWIDTHS, Job, RESOLUTIONS};

use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ez_ffmpeg::AVMediaType;
use ez_ffmpeg::Frame;
use ez_ffmpeg::container_info::get_duration_us;
use ez_ffmpeg::filter::frame_filter::FrameFilter;
use ez_ffmpeg::filter::frame_filter_context::FrameFilterContext;
use ez_ffmpeg::filter::frame_pipeline_builder::FramePipelineBuilder;
use tracing::{error, info, warn};

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

/// Convert MP4 to multi-scale HLS (fMP4) with VP9+Opus (VOD preset),
/// CUDA hwdecode + (prefer) GPU scale; no Clone on Input/Output.
pub fn convert_with_scales(
    job: &Job,
    input: &Path,
    output: &Path,
    scales: &[(u32, u32)],
    progress_callbacker: Arc<ProgressCallBacker>,
) -> anyhow::Result<()> {
    use ez_ffmpeg::{FfmpegContext, Input, Output};

    let Job { id: job_id, crf } = job;
    let in_path = input.to_str().unwrap();

    let n = scales.len();

    // [GPU] scale_npp → hwdownload → yuv420p
    let (outs_npp, branches_npp): (String, String) = scales
        .iter()
        .enumerate()
        .map(|(i, (w, h))| {
            (
                format!("[out{i}]"),
                format!(";[out{i}]scale_npp={w}:{h},hwdownload,format=yuv420p[v{i}]"),
            )
        })
        .unzip();
    let graphs_gpu_npp = format!("[0:v]split={n}{outs_npp}{branches_npp}");

    // [GPU] scale_cuda → hwdownload → yuv420p
    let (outs_cuda, branches_cuda): (String, String) = scales
        .iter()
        .enumerate()
        .map(|(i, (w, h))| {
            (
                format!("[out{i}]"),
                format!(";[out{i}]scale_cuda={w}:{h},hwdownload,format=yuv420p[v{i}]"),
            )
        })
        .unzip();
    let graphs_gpu_cuda = format!("[0:v]split={n}{outs_cuda}{branches_cuda}");

    // [CPU] hwdownload → split → scale → yuv420p
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
    let graphs_cpu = format!("[0:v]hwdownload,format=nv12,split={n}{outs_cpu}{branches_cpu}");

    let build_outputs = || -> anyhow::Result<Vec<Output>> {
        scales
            .iter()
            .copied()
            .enumerate()
            .map(|(i, (w, h))| {
                let frame_pipeline: FramePipelineBuilder = AVMediaType::AVMEDIA_TYPE_AUDIO.into();
                let progress_filter = ProgressCallBackFilter::new(progress_callbacker.clone());
                let pipe = frame_pipeline.filter("progress", Box::new(progress_filter));

                let out_dir = output.join(w.to_string());
                std::fs::create_dir_all(&out_dir)?;

                let playlist_path = out_dir.join(format!("{job_id}.m3u8"));
                let output_str = out_dir.to_str().unwrap();
                let hls_segment_filename = format!("{output_str}/{job_id}-%03d.m4s");

                let (tc, tr) = vp9_tile_params(w, h);

                Ok(Output::from(playlist_path.to_str().unwrap())
                    .add_frame_pipeline(pipe)
                    .set_video_codec("libvpx-vp9")
                    .set_video_codec_opt("b", "0")
                    .set_video_codec_opt("crf", crf.to_string())
                    .set_video_codec_opt("row-mt", "1")
                    .set_video_codec_opt("tile-columns", tc.to_string())
                    .set_video_codec_opt("tile-rows", tr.to_string())
                    .set_video_codec_opt("cpu-used", "2")
                    .set_video_codec_opt("lag-in-frames", "25")
                    .set_video_codec_opt("auto-alt-ref", "1")
                    // .set_video_codec_opt("g", "240")
                    .set_audio_codec("libopus")
                    .set_audio_codec_opt("b", "128k")
                    .add_stream_map("0:a?")
                    .add_stream_map(format!("v{i}"))
                    // HLS(fMP4) VOD
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

    let run_once = |graph: &str| -> anyhow::Result<()> {
        let in0 = Input::from(in_path)
            .set_hwaccel("cuda")
            .set_hwaccel_output_format("cuda");
        let builder = FfmpegContext::builder()
            .input(in0)
            .filter_desc(graph.to_string())
            .outputs(build_outputs()?);
        builder.build()?.start()?.wait()?;
        Ok(())
    };

    // GPU(npp) → GPU(cuda) → CPU
    if let Err(e1) = run_once(&graphs_gpu_npp) {
        error!("GPU scale_npp failed: {e1:?}");
        if let Err(e2) = run_once(&graphs_gpu_cuda) {
            error!("GPU scale_cuda failed: {e2:?}, falling back to CPU scale...");
            let in0 = Input::from(in_path)
                .set_hwaccel("cuda")
                .set_hwaccel_output_format("cuda");
            let builder = FfmpegContext::builder()
                .input(in0)
                .filter_desc(graphs_cpu)
                .outputs(build_outputs()?);
            builder.build()?.start()?.wait()?;
        }
    }

    Ok(())
}

pub(crate) fn create_master_playlist(job: &Job, input: &Path, output: &Path) -> anyhow::Result<()> {
    let job_id = job.id();

    let input_str = input.to_str().unwrap();
    let total = get_duration_us(input_str).unwrap();
    info!(%job_id, "Duration: {total} us");

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
