use crate::job::JobGenerator;
use crate::{BANDWIDTHS, Job, RESOLUTIONS};

use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ez_ffmpeg::Frame;
use ez_ffmpeg::container_info::get_duration_us;
use ez_ffmpeg::filter::frame_filter::FrameFilter;
use ez_ffmpeg::filter::frame_filter_context::FrameFilterContext;
use ez_ffmpeg::filter::frame_pipeline_builder::FramePipelineBuilder;
use ez_ffmpeg::{AVMediaType, Input};
use ez_ffmpeg::{FfmpegContext, Output};
use tracing::{debug, error, info, warn};

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

/// Convert MP4 to a VP9 variant with specified scales.
pub fn convert_with_scales(
    job: &Job,
    input: &Path,
    output: &Path,
    scales: &[(u32, u32)],
    progress_callbacker: Arc<ProgressCallBacker>,
) -> anyhow::Result<()> {
    let Job { id: job_id, crf } = job;

    let outputs = scales
        .iter()
        .copied()
        .enumerate()
        .map(|(i, (w, _))| {
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
                .set_format_opt("hls_time", "4")
                .set_format_opt("hls_segment_type", "fmp4")
                .set_format_opt("hls_playlist_type", "vod")
                .set_format_opt("hls_fmp4_init_filename", format!("{job_id}-init.mp4"))
                .set_format_opt("hls_segment_filename", hls_segment_filename))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let target_scales = scales.len();
    let (outs, scales): (String, String) = scales
        .iter()
        .enumerate()
        .map(|(i, (w, h))| (format!("[out{i}]"), format!(";[out{i}]scale={w}:{h}[v{i}]")))
        .unzip();
    let graphs = format!("[0:v]split={target_scales}{outs}{scales}");
    debug!(%job_id, %crf, ?input, ?graphs, "Converting to HLS");

    let input = Input::from(input.to_str().unwrap());
    let builder = FfmpegContext::builder().input(input).filter_desc(graphs);
    Ok(builder.outputs(outputs).build()?.start()?.wait()?)
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
