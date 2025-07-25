use crate::{BANDWIDTHS, Job, RESOLUTIONS, TEMP_DIR, VIDEOS_DIR};

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use ez_ffmpeg::AVMediaType;
use ez_ffmpeg::Frame;
use ez_ffmpeg::container_info::get_duration_us;
use ez_ffmpeg::filter::frame_filter::FrameFilter;
use ez_ffmpeg::filter::frame_filter_context::FrameFilterContext;
use ez_ffmpeg::filter::frame_pipeline_builder::FramePipelineBuilder;
use ez_ffmpeg::{FfmpegContext, Output};
use ffmpeg_next::{Dictionary, format, media};
use tracing::{debug, error, info, warn};

pub(crate) async fn spawn_hls_job(job: Job, upload_path: PathBuf) -> anyhow::Result<()> {
    let temp_dir = PathBuf::from(TEMP_DIR).join(format!("tmp-{}", job.id()));
    let out_dir = PathBuf::from(VIDEOS_DIR).join(job.id());
    tokio::fs::create_dir_all(&temp_dir).await?;

    let input_path = upload_path.clone();
    tokio::task::spawn_blocking(move || {
        create_master_playlist(&job, &input_path, &temp_dir)?;
        std::fs::create_dir_all(VIDEOS_DIR)?;
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

/// Convert MP4 to a VP9 variant of a specific height.
pub fn convert_to_vp9_variant(
    job: &Job,
    input: &Path,
    output: &Path,
    target_width: u32,
    target_height: u32,
    frame_pipeline: FramePipelineBuilder,
) -> anyhow::Result<()> {
    let Job { id: job_id, crf } = job;
    debug!(%job_id, %crf, ?input, ?output, "Converting to {target_width}P VP9 variant");

    let output = Output::from(output.to_str().unwrap())
        .set_video_codec_opt("crf", crf.to_string())
        .add_frame_pipeline(frame_pipeline);
    Ok(FfmpegContext::builder()
        .input(input.to_str().unwrap())
        .filter_desc(format!("scale={target_width}:{target_height}"))
        .output(output)
        .build()?
        .start()?
        .wait()?)
}

pub(crate) fn create_master_playlist(job: &Job, input: &Path, output: &Path) -> anyhow::Result<()> {
    let job_id = job.id();
    let vp9 = PathBuf::from(TEMP_DIR).join(format!("tmp-vp9-{job_id}"));
    std::fs::create_dir_all(&vp9)?;

    let input_str = input.to_str().unwrap();
    let total = get_duration_us(input_str).unwrap();
    info!(%job_id, "Duration: {} us", total);

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

    let with_filters = RESOLUTIONS
        .iter()
        .map(|&(w, h)| {
            let frame_pipeline: FramePipelineBuilder = AVMediaType::AVMEDIA_TYPE_AUDIO.into();
            let progress_filter = ProgressCallBackFilter::new(w, progress_callbacker.dup());
            let pipe = frame_pipeline.filter("progress", Box::new(progress_filter));
            (w, h, pipe)
        })
        .collect::<Vec<_>>();

    use rayon::prelude::*;
    // use rayon::iter::ParallelIterator as _;
    with_filters
        .into_par_iter()
        .map(|(width, height, pipe)| {
            let job_c = job.clone();
            convert_to_target_hls(&job_c, input, &vp9, width, height, output, pipe).inspect_err(
                |error| error!(%error, "Failed to convert to {}x{}P HLS", width, height),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

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

pub(crate) fn convert_to_target_hls(
    job: &Job,
    input: &Path,
    temp_dir: &Path,
    target_width: u32,
    target_height: u32,
    out_dir: &Path,
    frame_pipeline: FramePipelineBuilder,
) -> anyhow::Result<()> {
    let job_id = job.id();
    let vp9_file = temp_dir.join(format!("{target_width}P.webm"));
    let out_dir = out_dir.join(target_width.to_string());
    std::fs::create_dir_all(&out_dir)?;

    // convert to vp9
    convert_to_vp9_variant(
        job,
        input,
        &vp9_file,
        target_width,
        target_height,
        frame_pipeline,
    )
    .inspect_err(|error| error!(%error, "Failed to convert to {target_width}P vp9"))?;
    let input = vp9_file;
    let input_path = input.to_str().ok_or(anyhow!("Empty input path"))?;

    debug!(%job_id, ?input, ?out_dir, "Converting to {target_width}P HLS");

    // input context
    let mut open_opts = Dictionary::new();
    open_opts.set("probesize", "5000000"); // Read 5 MB data to probe
    open_opts.set("analyzeduration", "10000000"); // Read 10 s data to analyze

    let mut ictx = format::input_with_dictionary(input_path, open_opts)
        .inspect_err(|error| error!(?error, %job_id, "Failed to open input vp9 video"))?;

    // output context
    let playlist_path = out_dir.join(format!("{job_id}.m3u8"));
    let mut octx = format::output_as(playlist_path.to_str().unwrap(), "hls")?;

    // set HLS options
    let mut opts = Dictionary::new();
    opts.set("hls_time", "4");
    opts.set("hls_playlist_type", "vod");
    opts.set(
        "hls_segment_filename",
        &format!(
            "{}/{job_id}-%03d.m4s",
            out_dir.to_str().ok_or(anyhow!("Empty output path"))?
        ),
    );
    opts.set("hls_fmp4_init_filename", &format!("{job_id}-init.mp4"));
    opts.set("hls_segment_type", "fmp4");

    // codec copy
    for stream in ictx.streams() {
        let codec_id = stream.parameters().id();
        let mut ost = octx.add_stream(codec_id)?;
        ost.set_parameters(stream.parameters());
    }

    octx.write_header_with(opts).map_err(|error| {
        anyhow!("Failed to write header with opts when convert to hls: {error}")
    })?;

    for (stream, mut packet) in ictx.packets() {
        if packet.duration() == 0 && stream.parameters().medium() == media::Type::Video {
            let fr = stream.avg_frame_rate();
            if fr.numerator() != 0 {
                let tb = stream.time_base();
                // duration = time_base_den/frame_rate_num ÷ (time_base_num/frame_rate_den)
                let dur = (tb.denominator() as i64 * fr.denominator() as i64)
                    / (tb.numerator() as i64 * fr.numerator() as i64);
                packet.set_duration(dur);
            }
        }

        packet.set_stream(stream.index());
        if let Some(octx_stream) = octx.stream(stream.index()) {
            // rescale timestamp
            packet.rescale_ts(stream.time_base(), octx_stream.time_base());
        }
        packet.write_interleaved(&mut octx)?;
    }
    octx.write_trailer()?;
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

    pub fn dup(&self) -> Self {
        Self {
            job_id: self.job_id.clone(),
            total_duration: self.total_duration,
            time_base: self.time_base,
            last_report: Arc::new(Mutex::new(0)),
        }
    }

    pub fn print_progress(&self, width: u32, frame: &Frame) {
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
                    width,
                    "Progress: {clamped:.2}% (Current: {time:.3}s / Total: {total:.2}s, PTS: {pts})"
                );
            }
        }
    }
}

pub struct ProgressCallBackFilter {
    width: u32,
    progress_callback: ProgressCallBacker,
}

impl ProgressCallBackFilter {
    pub fn new(width: u32, progress_callback: ProgressCallBacker) -> Self {
        Self {
            width,
            progress_callback,
        }
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
        self.progress_callback.print_progress(self.width, &frame);
        Ok(Some(frame))
    }
}
