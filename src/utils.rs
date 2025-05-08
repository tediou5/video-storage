use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{anyhow, bail};
use ffmpeg_next::format::Pixel::YUV420P;
use ffmpeg_next::format::Sample;
use ffmpeg_next::format::context::Output as OutputContext;
use ffmpeg_next::frame;
use ffmpeg_next::software::resampling::Context as SamplerContext;
use ffmpeg_next::software::scaling::context::Context as Scaler;
use ffmpeg_next::software::scaling::flag::Flags;
use ffmpeg_next::threading;
use ffmpeg_next::{ChannelLayout, Rational};
use ffmpeg_next::{Dictionary, Packet, codec, format, media};
use tracing::{debug, error, info, trace, warn};

const CRF: &str = "38";

// Opus target parameters
const OPUS_TARGET_FORMAT: Sample = Sample::F32(format::sample::Type::Packed);
const OPUS_TARGET_RATE: i32 = 48000; // 48kHz
const OPUS_TARGET_LAYOUT: ChannelLayout = ChannelLayout::STEREO;
const OPUS_TARGET_BITRATE: usize = 128_000; // 128kbps

static NUM_CPUS: LazyLock<usize> = LazyLock::new(|| {
    let n = num_cpus::get();
    let num = if n > 16 { 16 } else { n };
    info!(num, "Detecting CPU cores");
    num
});

// Helper function: checks if a Rational is valid (numerator and denominator are both > 0)
fn is_rational_valid(r: Rational) -> bool {
    r.numerator() > 0 && r.denominator() > 0
}

/// Gets a valid frame rate from the input video stream.
fn get_valid_frame_rate(in_video_stream: &ffmpeg_next::Stream) -> anyhow::Result<Rational> {
    // Use avg_frame_rate
    let avg_fps = in_video_stream.avg_frame_rate();
    debug!(
        "Frame Rate Acquisition: Input stream avg_frame_rate: {}/{}",
        avg_fps.numerator(),
        avg_fps.denominator()
    );
    if is_rational_valid(avg_fps) {
        debug!("Successfully acquired frame rate using avg_frame_rate.");
        return Ok(avg_fps);
    }

    // Use r_frame_rate (in_video_stream.rate())
    let r_fps = in_video_stream.rate(); // Corresponds to AVStream->r_frame_rate
    debug!(
        "Frame Rate Acquisition: Input stream r_frame_rate: {}/{}",
        r_fps.numerator(),
        r_fps.denominator()
    );
    if is_rational_valid(r_fps) {
        debug!("Successfully acquired frame rate using r_frame_rate.");
        return Ok(r_fps);
    }

    // If both attempts fail
    let error_message = format!(
        "Unable to determine a valid frame rate for the encoder. Both sources were invalid.\n\
         Details:\n\
         - in_video_stream.avg_frame_rate(): {}/{}\n\
         - in_video_stream.rate() (r_frame_rate): {}/{}",
        avg_fps.numerator(),
        avg_fps.denominator(),
        r_fps.numerator(),
        r_fps.denominator()
    );
    Err(anyhow!(error_message))
}

/// Gets a valid encoder timebase according to a prioritized strategy.
fn get_valid_encoder_timebase(
    in_video_stream: &ffmpeg_next::Stream,
    decoder_context: &ffmpeg_next::codec::decoder::Video,
) -> anyhow::Result<Rational> {
    // Input stream's time_base (in_video_stream.time_base())
    let stream_tb = in_video_stream.time_base();
    debug!(
        "Timebase Acquisition: Input stream time_base (in_video_stream.time_base()): {}/{}",
        stream_tb.numerator(),
        stream_tb.denominator()
    );
    if is_rational_valid(stream_tb) {
        debug!("Successfully acquired: Using in_video_stream.time_base()");
        return Ok(stream_tb);
    }

    // From input stream's r_frame_rate (in_video_stream.rate())
    let r_frame_rate = in_video_stream.rate(); // Corresponds to AVStream->r_frame_rate
    debug!(
        "Timebase Acquisition: Input stream r_frame_rate (in_video_stream.rate()): {}/{}",
        r_frame_rate.numerator(),
        r_frame_rate.denominator()
    );
    if is_rational_valid(r_frame_rate) {
        // Timebase is the reciprocal of frame rate
        let derived_tb = Rational::new(r_frame_rate.denominator(), r_frame_rate.numerator());
        debug!(
            "Derived time_base from r_frame_rate: {}/{}",
            derived_tb.numerator(),
            derived_tb.denominator()
        );
        // Re-check derived_tb to ensure no zero in numerator/denominator after inversion
        if is_rational_valid(derived_tb) {
            debug!("Successfully acquired: Using time_base derived from r_frame_rate");
            return Ok(derived_tb);
        }
    }

    // From input stream's avg_frame_rate (in_video_stream.avg_frame_rate())
    let avg_frame_rate = in_video_stream.avg_frame_rate(); // Corresponds to AVStream->avg_frame_rate
    debug!(
        "Timebase Acquisition: Input stream avg_frame_rate (in_video_stream.avg_frame_rate()): {}/{}",
        avg_frame_rate.numerator(),
        avg_frame_rate.denominator()
    );
    if is_rational_valid(avg_frame_rate) {
        // Timebase is the reciprocal of frame rate
        let derived_tb = Rational::new(avg_frame_rate.denominator(), avg_frame_rate.numerator());
        debug!(
            "Derived time_base from avg_frame_rate: {}/{}",
            derived_tb.numerator(),
            derived_tb.denominator()
        );
        // Re-check derived_tb
        if is_rational_valid(derived_tb) {
            debug!("Successfully acquired: Using time_base derived from avg_frame_rate");
            return Ok(derived_tb);
        }
    }

    // Decoder context's time_base (decoder_context.time_base())
    let decoder_tb = decoder_context.time_base();
    debug!(
        "Timebase Acquisition: Decoder context time_base (decoder_context.time_base()): {}/{}",
        decoder_tb.numerator(),
        decoder_tb.denominator()
    );
    if is_rational_valid(decoder_tb) {
        debug!("Successfully acquired: Using decoder_context.time_base()");
        return Ok(decoder_tb);
    }

    // If all attempts fail
    let error_message = format!(
        "Unable to determine a valid time_base for the encoder. All sources were invalid.\n\
         Details:\n\
         - in_video_stream.time_base() (Stream time_base):            {}/{}\n\
         - in_video_stream.rate() (Stream r_frame_rate):              {}/{}\n\
         - in_video_stream.avg_frame_rate() (Stream avg_frame_rate):  {}/{}\n\
         - decoder_context.time_base() (Decoder context time_base): {}/{}",
        stream_tb.numerator(),
        stream_tb.denominator(),
        r_frame_rate.numerator(),
        r_frame_rate.denominator(),
        avg_frame_rate.numerator(),
        avg_frame_rate.denominator(),
        decoder_tb.numerator(),
        decoder_tb.denominator()
    );

    Err(anyhow!(error_message))
}

#[allow(clippy::field_reassign_with_default)]
fn setup_vp9_video_encoder(
    job_id: &str,
    dec_video: &codec::decoder::Video,
    octx: &mut OutputContext,
    in_video_stream: &ffmpeg_next::Stream,
) -> anyhow::Result<(usize, codec::encoder::video::Encoder)> {
    debug!(%job_id, "Setting up VP9 video encoder...");

    let vp9_codec_finder = codec::encoder::find(codec::Id::VP9)
        .ok_or_else(|| anyhow!("VP9 Encoder: Codec not found"))?;

    let enc_config_builder = codec::Context::new_with_codec(vp9_codec_finder);
    let mut enc_config = enc_config_builder
        .encoder()
        .video()
        .map_err(|e| anyhow!("VP9 Encoder: Failed to create config: {e}"))?;

    enc_config.set_flags(codec::Flags::GLOBAL_HEADER);

    let mut threading_config = threading::Config::default();
    threading_config.count = *NUM_CPUS;
    threading_config.kind = threading::Type::Slice;
    enc_config.set_threading(threading_config);

    enc_config.set_format(YUV420P);
    enc_config.set_width(dec_video.width());
    enc_config.set_height(dec_video.height());

    let time_base = get_valid_encoder_timebase(in_video_stream, dec_video)?;
    enc_config.set_time_base(time_base);

    let frame_rate = get_valid_frame_rate(in_video_stream)?;
    enc_config.set_frame_rate(Some(frame_rate));

    debug!(%job_id, "VP9 Encoder: Timebase set to {}/{}, Frame rate set to {}/{}",
        time_base.numerator(), time_base.denominator(),
        frame_rate.numerator(), frame_rate.denominator());

    let mut opts = Dictionary::new();
    opts.set("crf", CRF);
    opts.set("b:v", "0");
    // VP9 options:
    // opts.set("row-mt", "1");
    // opts.set("deadline", "good");
    // opts.set("cpu-used", "4");

    let tb_before_open = enc_config.time_base();
    let fr_before_open = enc_config.frame_rate();
    debug!(%job_id, "VP9 Encoder: Params before open: time_base={}/{}, frame_rate={}/{}",
        tb_before_open.numerator(), tb_before_open.denominator(),
        fr_before_open.numerator(), fr_before_open.denominator());

    let opened_encoder = enc_config.open_with(opts).map_err(|e| {
        anyhow!(
            "VP9 Encoder: Failed to open: {e}. Input params were TB={}/{}, FR={}/{}.",
            tb_before_open.numerator(),
            tb_before_open.denominator(),
            fr_before_open.numerator(),
            fr_before_open.denominator()
        )
    })?;
    debug!(%job_id, "VP9 video encoder opened successfully.");

    let mut ost_video = octx.add_stream(vp9_codec_finder.id())?;
    ost_video.set_parameters(&opened_encoder);
    debug!(%job_id, "VP9 video stream added to output with index {}", ost_video.index());

    // Explicitly set the stream's own time_base (AVStream->time_base).
    ost_video.set_time_base(ffmpeg_next::Rational::new(1, 1000));

    Ok((ost_video.index(), opened_encoder))
}

fn setup_opus_audio_encoder_and_resampler(
    job_id: &str,
    dec_audio: &codec::decoder::Audio,
    octx: &mut OutputContext,
) -> anyhow::Result<(
    usize,
    codec::encoder::audio::Encoder,
    SamplerContext,
    usize,
    usize,
)> {
    info!(%job_id, "Setting up Opus audio encoder and resampler..."); // Assuming job_id not passed, use placeholder

    let opus_codec_finder = codec::encoder::find(codec::Id::OPUS).ok_or_else(|| {
        anyhow!("Opus Encoder: Codec not found (ensure libopus is part of your FFmpeg build)")
    })?;

    let enc_config_builder = codec::Context::new_with_codec(opus_codec_finder);
    let mut enc_config = enc_config_builder
        .encoder()
        .audio()
        .map_err(|e| anyhow!("Opus Encoder: Failed to create config: {e}"))?;

    enc_config.set_format(OPUS_TARGET_FORMAT);
    enc_config.set_rate(OPUS_TARGET_RATE);
    enc_config.set_channel_layout(OPUS_TARGET_LAYOUT);
    enc_config.set_bit_rate(OPUS_TARGET_BITRATE);
    enc_config.set_time_base(Rational::new(1, OPUS_TARGET_RATE)); // Standard for audio

    let opus_opts = Dictionary::new();
    // opts.set("application", "audio");
    // opts.set("vbr", "on");

    debug!(%job_id, "Opus Encoder: Params before open: \
        format={OPUS_TARGET_FORMAT:?}, \
        rate={OPUS_TARGET_RATE}, \
        layout={OPUS_TARGET_LAYOUT:?}, \
        time_base=1/{OPUS_TARGET_RATE}, \
        bitrate={OPUS_TARGET_BITRATE}"
    );

    let opened_encoder = enc_config
        .open_with(opus_opts)
        .map_err(|e| anyhow!("Opus Encoder: Failed to open: {e}"))?;
    info!(%job_id, "Opus audio encoder opened successfully.");

    let frame_size = opened_encoder.frame_size();
    let actual_opus_frame_size = if frame_size == 0 {
        warn!(%job_id, "Opus encoder reported frame_size 0. Defaulting to 960 (20ms @ 48kHz). This might be risky.");
        960
    } else {
        frame_size as usize
    };
    let actual_opus_channels = opened_encoder.channels() as usize;
    if actual_opus_channels == 0 {
        bail!("Opus Encoder: Reported 0 channels after opening, cannot proceed.");
    }
    debug!(%job_id, "Opus Encoder: Reported frame_size: {actual_opus_frame_size}, channels: {actual_opus_channels}");

    let mut ost_audio = octx.add_stream(opus_codec_finder.id())?;
    ost_audio.set_parameters(&opened_encoder);
    debug!(%job_id, "Opus audio stream added to output with index {}", ost_audio.index());

    // Explicitly set the stream's own time_base (AVStream->time_base)
    ost_audio.set_time_base(Rational::new(1, OPUS_TARGET_RATE)); // Assuming OPUS_TARGET_RATE is u32 or similar, cast to i32

    debug!(%job_id, "Opus audio stream (index {}) added. Stream time_base: {:?}",
        ost_audio.index(),
        ost_audio.time_base(), // Verify this is now 1/OPUS_TARGET_RATE
    );

    // --- Audio Resampler Setup ---
    let in_ch_layout = dec_audio.channel_layout();
    let mut in_ch_layout_valid = if in_ch_layout.is_empty() || in_ch_layout.channels() == 0 {
        debug!(%job_id, "Audio Resampler: Input audio channel layout is empty/invalid, using default for {} channels.", dec_audio.channels());
        ChannelLayout::default(dec_audio.channels().into())
    } else {
        in_ch_layout
    };
    if in_ch_layout_valid.channels() != dec_audio.channels() as i32 && dec_audio.channels() > 0 {
        warn!(%job_id, "Audio Resampler: \
            Channel layout ({in_ch_layout_valid:?}, {}ch) channel count mismatch \
            with decoder channel count ({}ch). Adjusting layout.",
            in_ch_layout_valid.channels(),
            dec_audio.channels()
        );
        in_ch_layout_valid = ChannelLayout::default(dec_audio.channels().into());
    }

    debug!(%job_id, "Audio Resampler: Input Config: format={:?}, rate={}, layout={in_ch_layout_valid:?} ({} channels from decoder)",
        dec_audio.format(), dec_audio.rate(), dec_audio.channels());
    debug!(%job_id, "Audio Resampler: Output Config: format={OPUS_TARGET_FORMAT:?}, rate={OPUS_TARGET_RATE}, layout={OPUS_TARGET_LAYOUT:?}");

    let resampler = SamplerContext::get(
        dec_audio.format(),
        in_ch_layout_valid,
        dec_audio.rate(),
        OPUS_TARGET_FORMAT,
        OPUS_TARGET_LAYOUT,
        OPUS_TARGET_RATE as u32,
    )
    .map_err(|e| anyhow!("Audio Resampler: Failed to create: {e}"))?;
    debug!(%job_id, "Audio resampler created.");

    Ok((
        ost_audio.index(),
        opened_encoder,
        resampler,
        actual_opus_frame_size,
        actual_opus_channels,
    ))
}

/// Handles an encoded video packet by rescaling its timestamps and writing it to the output context.
fn handle_encoded_video_packet(
    job_id: &str,
    encoded_packet: &mut Packet,
    output_stream_index: usize,
    encoder_output_time_base: Rational,
    output_format_context: &mut format::context::Output,
) -> anyhow::Result<()> {
    // Log the state of the packet as it comes from the encoder
    let original_pts_val = encoded_packet.pts();
    let original_dts_val = encoded_packet.dts();
    let original_duration_val = encoded_packet.duration();

    trace!(
        %job_id,
        PTS_raw = ?original_pts_val,
        DTS_raw = ?original_dts_val,
        Duration_raw = ?original_duration_val,
        Encoder_Output_TB = ?encoder_output_time_base,
        "Video packet FROM ENCODER"
    );

    // Set the stream index for the packet
    encoded_packet.set_stream(output_stream_index);

    // Get the target time_base from the output stream context
    let target_stream_time_base = output_format_context
        .stream(output_stream_index)
        .ok_or_else(|| {
            let message = format!("Failed to get output stream for index {output_stream_index}");
            error!(%job_id, %message);
            anyhow!(message)
        })?
        .time_base();

    // Ensure both source (encoder_output_time_base) and target time_bases are valid for rescaling
    if encoder_output_time_base.denominator() == 0 {
        let message = format!(
            "Source (encoder output) time_base {encoder_output_time_base:?} has a zero denominator, cannot rescale PTS."
        );
        error!(%job_id, %message);
        return Err(anyhow!(message));
    }
    if target_stream_time_base.denominator() == 0 {
        // This check should ideally be redundant if prior setup guarantees a valid target_stream_time_base (like 1/1000)
        let message = format!(
            "Target (output stream) time_base {target_stream_time_base:?} has a zero denominator, cannot rescale PTS."
        );
        error!(%job_id, %message);
        return Err(anyhow!(message));
    }

    trace!(%job_id, ?encoder_output_time_base, ?target_stream_time_base, "Video RESCALE_TS");

    // Perform the timestamp rescaling
    encoded_packet.rescale_ts(encoder_output_time_base, target_stream_time_base);

    // Log the state of the packet after rescaling
    let final_pts_val = encoded_packet.pts();
    let final_dts_val = encoded_packet.dts();
    let final_duration_val = encoded_packet.duration();

    trace!(
        %job_id,
        PTS_final = ?final_pts_val,
        DTS_final = ?final_dts_val,
        Duration_final = ?final_duration_val,
        Target_Stream_TB = ?target_stream_time_base,
        "Video packet AFTER RESCALE_TS",
    );

    // Log PTS in seconds for easier interpretation
    if let Some(pts) = final_pts_val {
        // Check denominator again just for this calculation, as rescale_ts might produce strange rationals if inputs were weird
        if target_stream_time_base.denominator() != 0 {
            let pts_in_seconds = pts as f64 * target_stream_time_base.numerator() as f64
                / target_stream_time_base.denominator() as f64;
            trace!(%job_id, "Video packet AFTER RESCALE_TS: PTS_final_seconds={pts_in_seconds:.3}s");
        } else if target_stream_time_base.numerator() == 0
            && target_stream_time_base.denominator() == 1
        {
            // Handle case like Rational(0,1) which is valid but results in 0 seconds.
            trace!(%job_id, "Video packet AFTER RESCALE_TS: PTS_final_seconds=0.000s (Target_Stream_TB is 0/1)");
        } else {
            // This case should have been caught by the denominator check above before rescale_ts.
            warn!(
                %job_id,
                "Cannot calculate PTS in seconds for logging because Target_Stream_TB denominator is zero (was {target_stream_time_base:?})."
            );
        }
    }

    // Write the packet to the output file
    if let Err(e) = encoded_packet.write_interleaved(output_format_context) {
        let message = format!("Output: Error writing interleaved video packet: {e}");
        error!(%job_id, %message);
        return Err(anyhow!(message));
    }

    Ok(())
}

/// Handles an encoded audio packet: sets metadata, logs, and writes to output.
fn handle_encoded_audio_packet(
    job_id: &str,
    encoded_packet: &mut Packet,
    output_stream_index: usize,
    current_pts_in_source_tb: i64,     // audio_next_pts
    packet_duration_in_source_tb: i64, // opus_frame_size
    output_format_context: &mut OutputContext,
) -> anyhow::Result<()> {
    encoded_packet.set_stream(output_stream_index);

    encoded_packet.set_pts(Some(current_pts_in_source_tb));
    encoded_packet.set_dts(Some(current_pts_in_source_tb));
    encoded_packet.set_duration(packet_duration_in_source_tb);

    let source_audio_time_base = Rational::new(1, OPUS_TARGET_RATE);
    let target_webm_audio_time_base = Rational::new(1, 1000);

    trace!(
        %job_id,
        Raw_PTS = current_pts_in_source_tb,
        Raw_Duration = packet_duration_in_source_tb,
        Source_TB = ?source_audio_time_base,
        Source_TB = ?source_audio_time_base,
        Target_TB = ?target_webm_audio_time_base,
        "Audio packet BEFORE RESCALE",
    );

    encoded_packet.rescale_ts(source_audio_time_base, target_webm_audio_time_base);

    let final_pts = encoded_packet.pts().unwrap_or(0);
    let final_duration = encoded_packet.duration();
    let pts_in_seconds = if target_webm_audio_time_base.denominator() != 0 {
        final_pts as f64 * target_webm_audio_time_base.numerator() as f64
            / target_webm_audio_time_base.denominator() as f64
    } else {
        0.0
    };

    trace!(
        %job_id,
        "Audio packet AFTER RESCALE (PREPARED for writing): Final_PTS={}, Final_Duration={}, PTS_seconds={:.3}s, Target_TB={:?}",
        final_pts,
        final_duration,
        pts_in_seconds,
        target_webm_audio_time_base
    );

    // 5. 写入数据包
    if let Err(e) = encoded_packet.write_interleaved(output_format_context) {
        let error = format!(
            "Output: Error writing interleaved audio packet (Rescaled PTS {final_pts}): {e}",
        );
        error!(%job_id, %error);
        return Err(anyhow!(error));
    }

    Ok(())
}

fn flush_video_path(
    job_id: &str,
    dec_video: &mut codec::decoder::Video,
    scaler: &mut Scaler,
    enc_video: &mut codec::encoder::Video,
    octx: &mut OutputContext,
    in_video_time_base: Rational,
    out_video_stream_idx: usize,
) -> anyhow::Result<()> {
    debug!(%job_id, "Flushing video decoder and encoder...");

    // Drain the decoder
    debug!(%job_id, "Sending EOF to video decoder...");
    dec_video.send_eof()?;
    let mut decoded_video_frame_eof = frame::Video::empty();
    let mut scaled_video_frame_eof = frame::Video::empty();
    loop {
        match dec_video.receive_frame(&mut decoded_video_frame_eof) {
            Ok(_) => {
                // Frame successfully received from decoder
                match scaler.run(&decoded_video_frame_eof, &mut scaled_video_frame_eof) {
                    Ok(_) => {
                        scaled_video_frame_eof.set_pts(decoded_video_frame_eof.pts());
                        if let Err(e) = enc_video.send_frame(&scaled_video_frame_eof) {
                            // EAGAIN might happen if encoder needs draining, but we drain below anyway.
                            // Log other errors.
                            if e != (ffmpeg_next::Error::Other {
                                errno: ffmpeg_next::util::error::EAGAIN,
                            }) {
                                warn!(%job_id, "VP9 Encoder: Error sending flushed frame: {e}, skipping.");
                            }
                        }
                    }
                    Err(e) => {
                        warn!(%job_id, "Video Scaler: Error during flush: {e}, skipping frame.")
                    }
                }
                // Try draining the encoder immediately after sending a frame
                // This loop structure assumes linear draining. Complex interactions might need different logic.
                let mut encoded_video_packet = Packet::empty();
                while enc_video.receive_packet(&mut encoded_video_packet).is_ok() {
                    encoded_video_packet.set_stream(out_video_stream_idx);
                    encoded_video_packet.rescale_ts(
                        in_video_time_base,
                        octx.stream(out_video_stream_idx).unwrap().time_base(),
                    );
                    if encoded_video_packet.write_interleaved(octx).is_err() {
                        error!(%job_id, "Output: Error writing flushed interleaved video packet, stopping.");
                        return Err(anyhow!(
                            "Output: Error writing flushed interleaved video packet"
                        ));
                    }
                }
                // Ignore EAGAIN from receive_packet here, continue draining decoder
            }
            Err(ffmpeg_next::Error::Eof) => {
                // <-- CORRECT EOF Check
                debug!(%job_id, "Video decoder fully flushed (EOF received).");
                break; // Exit the decoder drain loop
            }
            Err(e) => {
                // Handle other real errors
                error!(%job_id, "Video Decoder: Error receiving flushed frame: {}", e);
                return Err(e.into());
            }
        }
    } // End decoder drain loop

    // Send EOF to the encoder and drain it completely
    debug!(%job_id, "Sending EOF to video encoder...");
    enc_video.send_eof()?;
    let mut encoded_video_packet_eof = Packet::empty();
    loop {
        match enc_video.receive_packet(&mut encoded_video_packet_eof) {
            Ok(_) => {
                if let Err(error) = handle_encoded_video_packet(
                    job_id,
                    &mut encoded_video_packet_eof,
                    out_video_stream_idx,
                    in_video_time_base, // Use the passed encoder's output time_base
                    octx,
                ) {
                    // Log error, but might continue flushing other packets unless it's a fatal write error
                    error!(%job_id, ?error, "Error handling flushed video packet");
                    // Decide if to propagate or just log
                }
            }
            Err(ffmpeg_next::Error::Eof) => {
                // <-- CORRECT EOF Check
                debug!(%job_id, "Video encoder fully flushed (EOF received).");
                break; // Exit the encoder drain loop
            }
            Err(error) => {
                // Handle other real errors
                error!(%job_id, ?error, "VP9 Encoder: Error receiving final packet");
                return Err(error.into());
            }
        }
    } // End encoder drain loop

    debug!(%job_id, "Video path flushed successfully.");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn flush_audio_path(
    job_id: &str,
    dec_audio: &mut codec::decoder::Audio,
    audio_resampler: &mut SamplerContext,
    enc_audio: &mut codec::encoder::Audio,
    octx: &mut OutputContext,
    out_audio_stream_idx: usize,
    audio_sample_buffer: &mut Vec<f32>,
    opus_input_frame: &mut frame::Audio,
    opus_frame_size: usize,
    opus_num_channels: usize,
    audio_next_pts: &mut i64,
) -> anyhow::Result<()> {
    debug!(
        %job_id,
        "Flushing audio path starting. Initial audio_next_pts: {} ({:.3}s)",
        *audio_next_pts, *audio_next_pts as f64 / OPUS_TARGET_RATE as f64
    );

    // 1. Drain the audio decoder
    debug!(%job_id, "Sending EOF to audio decoder...");
    dec_audio.send_eof()?;
    let mut decoded_audio_frame_eof = frame::Audio::empty();
    loop {
        match dec_audio.receive_frame(&mut decoded_audio_frame_eof) {
            Ok(_) => {
                // Frame received
                let mut temp_resampled_frame_eof = frame::Audio::empty();
                if audio_resampler
                    .run(&decoded_audio_frame_eof, &mut temp_resampled_frame_eof)
                    .is_ok()
                    && temp_resampled_frame_eof.samples() > 0
                {
                    // Append data to buffer
                    let num_new_samples = temp_resampled_frame_eof.samples();
                    let samples_ptr = temp_resampled_frame_eof.data(0).as_ptr() as *const f32;
                    let new_data_slice = unsafe {
                        std::slice::from_raw_parts(samples_ptr, num_new_samples * opus_num_channels)
                    };
                    audio_sample_buffer.extend_from_slice(new_data_slice);
                }
            }
            Err(ffmpeg_next::Error::Eof) => {
                // <-- CORRECT EOF Check
                debug!(%job_id, "Audio decoder fully flushed (EOF received).");
                break; // Exit decoder drain loop
            }
            Err(e) => {
                error!(%job_id, "Audio Decoder: Error receiving flushed frame: {}", e);
                return Err(e.into());
            }
        }
    } // End decoder drain loop

    // 2. Flush the resampler (Simplified version - see previous notes)
    // ... (resampler flush logic remains the same conceptually) ...
    debug!(%job_id, "Attempting to flush audio resampler (simplified method)...");
    let mut flushed_samples = true;
    let empty_input_for_flush =
        frame::Audio::new(dec_audio.format(), 0, dec_audio.channel_layout());
    while flushed_samples {
        let mut temp_resampled_frame_eof = frame::Audio::empty();
        if audio_resampler
            .run(&empty_input_for_flush, &mut temp_resampled_frame_eof)
            .is_ok()
            && temp_resampled_frame_eof.samples() > 0
        {
            let num_new_samples = temp_resampled_frame_eof.samples();
            let samples_ptr = temp_resampled_frame_eof.data(0).as_ptr() as *const f32;
            let new_data_slice = unsafe {
                std::slice::from_raw_parts(samples_ptr, num_new_samples * opus_num_channels)
            };
            audio_sample_buffer.extend_from_slice(new_data_slice);
            flushed_samples = true;
        } else {
            flushed_samples = false;
        }
    }
    debug!(%job_id, "Audio resampler flushing attempted. Buffer size for final processing: {} samples per channel", audio_sample_buffer.len() / opus_num_channels);

    // 3. Process all full frames remaining in the buffer
    while audio_sample_buffer.len() >= opus_frame_size * opus_num_channels {
        // ... (copy data to opus_input_frame) ...
        let samples_for_opus_frame_len = opus_frame_size * opus_num_channels;
        let plane_data_mut = opus_input_frame.data_mut(0);
        unsafe {
            let dest_ptr = plane_data_mut.as_mut_ptr() as *mut f32;
            let src_ptr = audio_sample_buffer.as_ptr();
            std::ptr::copy_nonoverlapping(src_ptr, dest_ptr, samples_for_opus_frame_len);
        }
        audio_sample_buffer.drain(0..samples_for_opus_frame_len);
        opus_input_frame.set_pts(None);

        if let Err(e) = enc_audio.send_frame(opus_input_frame) {
            /* handle error */
            return Err(anyhow!(
                "Failed sending buffered audio frame during flush: {e}",
            ));
        }

        let mut encoded_audio_packet = Packet::empty();
        // Inner drain loop for encoder after sending one frame
        loop {
            match enc_audio.receive_packet(&mut encoded_audio_packet) {
                Ok(_) => {
                    handle_encoded_audio_packet(
                        job_id,
                        &mut encoded_audio_packet,
                        out_audio_stream_idx,
                        *audio_next_pts,
                        opus_frame_size as i64,
                        octx,
                    )?;
                    *audio_next_pts += opus_frame_size as i64;
                }
                Err(ffmpeg_next::Error::Other { errno })
                    if errno == ffmpeg_next::util::error::EAGAIN =>
                {
                    // Encoder needs more input or is finishing this frame batch. Break inner loop.
                    break;
                }
                Err(ffmpeg_next::Error::Eof) => {
                    // This shouldn't happen here as we haven't sent EOF to encoder yet. Log warning/error.
                    warn!(%job_id, "Opus Encoder: Received EOF unexpectedly while draining buffer.");
                    break; // Treat as end for this inner loop
                }
                Err(e) => {
                    /* handle other errors */
                    return Err(anyhow!(
                        "Opus Encoder: Error receiving packet while draining buffer: {}",
                        e
                    ));
                }
            }
        } // End inner encoder drain loop
    } // End while buffer has full frames

    // 4. Handle the very last partial frame
    if !audio_sample_buffer.is_empty() {
        // ... (padding logic remains the same conceptually) ...
        let remaining_sample_count_per_channel = audio_sample_buffer.len() / opus_num_channels;
        debug!(%job_id, "Processing final {} audio samples per channel by padding.", remaining_sample_count_per_channel);
        let total_f32s_in_opus_frame = opus_frame_size * opus_num_channels;
        let mut final_frame_samples: Vec<f32> = Vec::with_capacity(total_f32s_in_opus_frame);
        final_frame_samples.extend_from_slice(audio_sample_buffer);
        let padding_needed_f32s = total_f32s_in_opus_frame - final_frame_samples.len();
        if padding_needed_f32s > 0 {
            final_frame_samples.resize(total_f32s_in_opus_frame, 0.0f32);
        }

        let plane_data_mut = opus_input_frame.data_mut(0);
        if plane_data_mut.len() < final_frame_samples.len() * std::mem::size_of::<f32>() {
            return Err(anyhow!(
                "Opus input frame buffer allocation mismatch during final padding"
            ));
        }

        debug!(%job_id, "Performing unsafe copy of final padded samples to frame plane...");
        unsafe {
            /* copy data */
            std::ptr::copy_nonoverlapping(
                final_frame_samples.as_ptr(),
                plane_data_mut.as_mut_ptr() as *mut f32,
                total_f32s_in_opus_frame,
            );
        }
        debug!(%job_id, "Unsafe copy for final frame completed.");
        opus_input_frame.set_pts(None);

        if let Err(e) = enc_audio.send_frame(opus_input_frame) {
            /* handle error */
            return Err(anyhow!("Failed sending final audio frame: {}", e));
        }

        // Drain the encoder after sending final frame
        let mut encoded_audio_packet = Packet::empty();
        loop {
            // Use loop with match for draining
            match enc_audio.receive_packet(&mut encoded_audio_packet) {
                Ok(_) => {
                    handle_encoded_audio_packet(
                        job_id,
                        &mut encoded_audio_packet,
                        out_audio_stream_idx,
                        *audio_next_pts,
                        opus_frame_size as i64,
                        octx,
                    )?;
                    *audio_next_pts += opus_frame_size as i64;
                }
                Err(ffmpeg_next::Error::Other { errno })
                    if errno == ffmpeg_next::util::error::EAGAIN =>
                {
                    // Expected when draining after sending one frame might not yield packet immediately
                    break;
                }
                Err(ffmpeg_next::Error::Eof) => {
                    warn!(%job_id, "Opus Encoder: Received EOF unexpectedly while draining final padded frame.");
                    break;
                }
                Err(e) => {
                    return Err(anyhow!(
                        "Opus Encoder: Error receiving packet after final padded frame: {e}"
                    ));
                }
            }
        } // End drain after final padded frame
    }
    // Clear buffer regardless
    audio_sample_buffer.clear();

    // 5. Send EOF to the audio encoder and drain it completely
    debug!(%job_id, "Sending EOF to Opus audio encoder...");
    enc_audio.send_eof()?;
    let mut encoded_audio_packet_eof = Packet::empty();
    let mut encoder_final_drain_packets = 0;
    loop {
        match enc_audio.receive_packet(&mut encoded_audio_packet_eof) {
            Ok(_) => {
                encoder_final_drain_packets += 1;
                let packet_duration = if encoded_audio_packet_eof.duration() > 0 {
                    encoded_audio_packet_eof.duration()
                } else {
                    opus_frame_size as i64
                };
                handle_encoded_audio_packet(
                    job_id,
                    &mut encoded_audio_packet_eof,
                    out_audio_stream_idx,
                    *audio_next_pts,
                    packet_duration, // Use determined packet_duration
                    octx,
                )?;

                trace!(
                    %job_id,
                    "Audio packet from Step 5 (encoder EOF drain, packet #{}): audio_next_pts={}, PTS_seconds={:.3}s, packet_duration_used={}",
                    encoder_final_drain_packets,
                    *audio_next_pts,
                    *audio_next_pts as f64 / OPUS_TARGET_RATE as f64,
                    packet_duration
                );
                *audio_next_pts += packet_duration;
            }
            Err(ffmpeg_next::Error::Eof) => {
                // <-- CORRECT EOF Check
                debug!(%job_id, "Opus audio encoder fully flushed (EOF received).");
                break; // Exit encoder drain loop
            }
            Err(e) => {
                // Handle other real errors
                error!(%job_id, "Opus Encoder: Error receiving final packet: {}", e);
                return Err(e.into());
            }
        }
    } // End encoder EOF drain loop

    debug!(
        %job_id,
        "Audio path flushed successfully. Final audio_next_pts value after all processing: {} ({:.3}s).",
        *audio_next_pts, *audio_next_pts as f64 / OPUS_TARGET_RATE as f64
    );
    Ok(())
}

/// Convert MP4 to VP9
pub fn convert_to_vp9(job_id: &str, input: &Path, output: &Path) -> anyhow::Result<()> {
    debug!(%job_id, ?input, ?output, crf = %CRF, "Converting to VP9");
    let mut ictx = format::input(input.to_str().unwrap())
        .map_err(|e| anyhow!("Failed to open input video: {e}"))?;
    let mut octx = format::output_as(output.to_str().unwrap(), "webm")
        .map_err(|e| anyhow!("Failed to create ouput context: {e}"))?;

    let in_video = ictx
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| anyhow!("Can not find input video stream"))?;
    let in_audio = ictx
        .streams()
        .best(media::Type::Audio)
        .ok_or_else(|| anyhow!("Can not find input audio stream"))?;
    let in_video_idx = in_video.index();
    let in_audio_idx = in_audio.index();

    let codec_params = in_video.parameters();
    let codec = codec::context::Context::from_parameters(codec_params)?;
    let mut dec_video = codec.decoder().video()?;

    let codec_params = in_audio.parameters();
    let codec = codec::context::Context::from_parameters(codec_params)?;
    let mut dec_audio = codec.decoder().audio()?;

    let mut scaler = Scaler::get(
        dec_video.format(),
        dec_video.width(),
        dec_video.height(),
        YUV420P,
        dec_video.width(),
        dec_video.height(),
        Flags::BILINEAR,
    )?;

    // Setup video encoder
    let (out_video_stream_idx, mut enc_video) =
        setup_vp9_video_encoder(job_id, &dec_video, &mut octx, &in_video)?;

    // Setup audio encoder and resampler
    let (
        out_audio_stream_idx,
        mut enc_audio,
        mut audio_resampler,
        opus_frame_size,
        opus_num_channels,
    ) = setup_opus_audio_encoder_and_resampler(job_id, &dec_audio, &mut octx)?;

    debug!(
        "Output audio stream time_base before write_header: {:?}",
        octx.stream(out_audio_stream_idx).unwrap().time_base()
    );
    debug!(
        "Output video stream time_base before write_header: {:?}",
        octx.stream(out_video_stream_idx).unwrap().time_base()
    );

    octx.write_header()
        .map_err(|e| anyhow!("Output: Failed to write context header: {e}"))?;
    debug!("Output context header written successfully.");

    let in_video_time_base = in_video.time_base();
    let mut decoded_video_frame = frame::Video::empty();
    let mut scaled_video_frame = frame::Video::empty();
    let mut decoded_audio_frame = frame::Audio::empty();
    let mut audio_sample_buffer: Vec<f32> = Vec::new();
    let mut opus_input_frame = frame::Audio::new(
        OPUS_TARGET_FORMAT, // Use const
        opus_frame_size,
        OPUS_TARGET_LAYOUT, // Use const
    );

    info!(%job_id, "Starting packet processing loop. Opus frame size: {}, Opus channels: {}", opus_frame_size, opus_num_channels);

    // Initialize PTS counter for audio
    let mut audio_next_pts: i64 = 0;
    let total_duration_us = ictx.duration();
    let mut last_reported_progress_video_ts_us: i64 = 0;
    const PROGRESS_INTERVAL_US: i64 = 10_000_000; // 10 seconds in microseconds

    'main_loop: for (stream, packet) in ictx.packets() {
        if stream.index() == in_video_idx {
            // ... (video processing logic as in your last full code) ...
            if dec_video.send_packet(&packet).is_err() {
                warn!(%job_id, "Video Decoder: Error sending packet, skipping.");
                continue;
            }
            while dec_video.receive_frame(&mut decoded_video_frame).is_ok() {
                if let Some(pts) = decoded_video_frame.pts() {
                    // Convert current frame's PTS to microseconds
                    let current_video_ts_us =
                        (pts * in_video_time_base.numerator() as i64 * 1_000_000)
                            / in_video_time_base.denominator() as i64;

                    if current_video_ts_us
                        >= last_reported_progress_video_ts_us + PROGRESS_INTERVAL_US
                    {
                        if total_duration_us > 0 {
                            let percent = (current_video_ts_us as f64 / total_duration_us as f64
                                * 100.0)
                                .min(100.0);
                            let current_s = current_video_ts_us as f64 / 1_000_000.0;
                            let total_s = total_duration_us as f64 / 1_000_000.0;
                            info!(
                                %job_id,
                                "Conversion Progress: {:.1}% ({:.1}s / {:.1}s processed)",
                                percent, current_s, total_s
                            );
                        } else {
                            // Total duration unknown, just print current time processed
                            let current_s = current_video_ts_us as f64 / 1_000_000.0;
                            info!(%job_id, "Conversion Progress: {:.1}s processed", current_s);
                        }
                        // Update last_reported_progress_video_ts_us to the boundary that was just crossed
                        last_reported_progress_video_ts_us =
                            (current_video_ts_us / PROGRESS_INTERVAL_US) * PROGRESS_INTERVAL_US;
                    }
                }

                if scaler
                    .run(&decoded_video_frame, &mut scaled_video_frame)
                    .is_err()
                {
                    warn!(%job_id, "Video Scaler: Error, skipping frame.");
                    continue;
                }
                scaled_video_frame.set_pts(decoded_video_frame.pts());
                if enc_video.send_frame(&scaled_video_frame).is_err() {
                    warn!(%job_id, "VP9 Encoder: Error sending frame, skipping.");
                    continue;
                }
                let mut encoded_video_packet = Packet::empty();
                while enc_video.receive_packet(&mut encoded_video_packet).is_ok() {
                    if let Err(error) = handle_encoded_video_packet(
                        job_id,
                        &mut encoded_video_packet,
                        out_video_stream_idx,
                        in_video_time_base,
                        &mut octx,
                    ) {
                        error!(%job_id, ?error, "Failed to handle encoded video packet, Stopping.");
                        // Consider breaking the main loop or handling the error appropriately
                        break 'main_loop; // Make sure 'main_loop is the correct label for your outer loop
                    }
                }
            }
        } else if stream.index() == in_audio_idx {
            if dec_audio.send_packet(&packet).is_err() {
                warn!(%job_id, "Audio Decoder: Error sending packet, skipping.");
                continue;
            }
            while dec_audio.receive_frame(&mut decoded_audio_frame).is_ok() {
                let mut temp_resampled_frame = frame::Audio::empty();
                match audio_resampler.run(&decoded_audio_frame, &mut temp_resampled_frame) {
                    Ok(_) => {
                        if temp_resampled_frame.samples() > 0 {
                            let num_new_samples = temp_resampled_frame.samples();
                            let samples_ptr = temp_resampled_frame.data(0).as_ptr() as *const f32;
                            let new_data_slice = unsafe {
                                std::slice::from_raw_parts(
                                    samples_ptr,
                                    num_new_samples * opus_num_channels,
                                )
                            };
                            audio_sample_buffer.extend_from_slice(new_data_slice);
                        }
                    }
                    Err(e) => {
                        warn!(%job_id, "Audio Resampler: Error during run: {e}, skipping this audio data.");
                        continue;
                    }
                }
                while audio_sample_buffer.len() >= opus_frame_size * opus_num_channels {
                    let samples_for_opus_frame_len = opus_frame_size * opus_num_channels;
                    let plane_data_mut = opus_input_frame.data_mut(0);
                    unsafe {
                        let dest_ptr = plane_data_mut.as_mut_ptr() as *mut f32;
                        let src_ptr = audio_sample_buffer.as_ptr();
                        std::ptr::copy_nonoverlapping(
                            src_ptr,
                            dest_ptr,
                            samples_for_opus_frame_len,
                        );
                    }
                    audio_sample_buffer.drain(0..samples_for_opus_frame_len);
                    opus_input_frame.set_pts(None);
                    if enc_audio.send_frame(&opus_input_frame).is_err() {
                        warn!(%job_id, "Opus Encoder: Error sending frame, data might be lost.");
                        break;
                    }
                    let mut encoded_audio_packet = Packet::empty();
                    while enc_audio.receive_packet(&mut encoded_audio_packet).is_ok() {
                        if let Err(error) = handle_encoded_audio_packet(
                            job_id,
                            &mut encoded_audio_packet,
                            out_audio_stream_idx,
                            audio_next_pts, // Pass the current value of audio_next_pts
                            opus_frame_size as i64, // Standard duration for Opus frames
                            &mut octx,
                        ) {
                            error!(%job_id, ?error, "Output: Error writing interleaved audio packet, stopping.");
                            break 'main_loop;
                        };
                        audio_next_pts += opus_frame_size as i64; // Increment PTS for the next packet
                    }
                }
            }
        }
    }
    // Final progress log
    if total_duration_us > 0 {
        let total_s_final = total_duration_us as f64 / 1_000_000.0;
        info!(%job_id, "Conversion Progress: 100.0% ({:.1}s / {:.1}s processed) (finalizing)", total_s_final, total_s_final);
    }

    debug!(%job_id, "Flushing remaining frames...");
    flush_video_path(
        job_id,
        &mut dec_video,
        &mut scaler,
        &mut enc_video,
        &mut octx,
        in_video_time_base,
        out_video_stream_idx,
    )?;
    flush_audio_path(
        job_id,
        &mut dec_audio,
        &mut audio_resampler,
        &mut enc_audio,
        &mut octx,
        out_audio_stream_idx,
        &mut audio_sample_buffer,
        &mut opus_input_frame,
        opus_frame_size,
        opus_num_channels,
        &mut audio_next_pts,
    )?;

    octx.write_trailer()
        .map_err(|e| anyhow!("Output: Failed to write trailer: {e}"))?;
    info!(%job_id, ?output, "Conversion to WebM (VP9/Opus) completed successfully.");

    Ok(())
}

pub(crate) fn convert_to_hls(job_id: &str, input: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let temp_vp9_dir = PathBuf::from("/tmp").join(format!("tmp-vp9-{job_id}"));
    std::fs::create_dir_all(&temp_vp9_dir)?;

    let vp9_file = temp_vp9_dir.join("vp9.webm");
    // convert to vp9
    convert_to_vp9(job_id, input, &vp9_file)
        .inspect_err(|error| error!(%error, "Failed to convert to vp9"))?;
    let input = vp9_file;
    let input_path = input.to_str().ok_or(anyhow!("Empty input path"))?;

    debug!(%job_id, ?input, ?out_dir, "Converting to HLS");

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
            "{}/segment_%03d.m4s",
            out_dir.to_str().ok_or(anyhow!("Empty output path"))?
        ),
    );
    opts.set("hls_fmp4_init_filename", "init.mp4");
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
