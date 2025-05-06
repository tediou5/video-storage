use std::path::{Path, PathBuf};

use anyhow::anyhow;
use ffmpeg_next::format::Pixel::YUV420P;
use ffmpeg_next::{
    Dictionary, Packet, codec, format,
    frame::Video,
    media,
    software::scaling::{context::Context as Scaler, flag::Flags},
};
use tracing::error;

/// Convert MP4 to VP9
pub fn convert_to_vp9(input: &Path, output: &Path) -> anyhow::Result<()> {
    let mut ictx = format::input(input.to_str().unwrap())
        .map_err(|e| anyhow!("Failed to open input video: {e}"))?;
    let mut octx = format::output_as(output.to_str().unwrap(), "mp4")
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

    let mut dec_video = in_video.codec().decoder().video()?;
    // let mut dec_audio = in_audio.codec().decoder().audio()?;

    let mut scaler = Scaler::get(
        dec_video.format(),
        dec_video.width(),
        dec_video.height(),
        YUV420P,
        dec_video.width(),
        dec_video.height(),
        Flags::BILINEAR,
    )?;

    // Set output video codec
    let (video_stream_idx, mut vp9_enc) = {
        let mut ost = octx.add_stream(codec::Id::VP9)?;
        let mut enc = ost.codec().encoder().video()?;
        enc.set_width(dec_video.width());
        enc.set_height(dec_video.height());
        enc.set_format(YUV420P);
        enc.set_time_base(dec_video.time_base());
        enc.set_quality(30);
        enc.set_bit_rate(0);
        ost.set_parameters(&enc);
        (ost.index(), enc)
    };

    let audio_stream_idx = {
        let mut ost = octx.add_stream(in_audio.parameters().id())?;
        ost.set_parameters(in_audio.parameters());
        ost.index()
    };

    octx.write_header()?;

    for (stream, mut packet) in ictx.packets() {
        if stream.index() == in_video_idx {
            // decode → scale → encode → mux
            dec_video.send_packet(&packet)?;
            let mut decoded = Video::empty();
            while dec_video.receive_frame(&mut decoded).is_ok() {
                let mut yuv = Video::empty();
                scaler.run(&decoded, &mut yuv)?;
                vp9_enc.send_frame(&yuv)?;
                let mut out_pkt = Packet::empty();
                while vp9_enc.receive_packet(&mut out_pkt).is_ok() {
                    out_pkt.set_stream(video_stream_idx);
                    out_pkt.rescale_ts(
                        dec_video.time_base(),
                        octx.stream(video_stream_idx).unwrap().time_base(),
                    );
                    out_pkt.write_interleaved(&mut octx)?;
                }
            }
        } else if stream.index() == in_audio_idx {
            // copy audio stream
            packet.set_stream(audio_stream_idx);
            packet.write_interleaved(&mut octx)?;
        }
    }

    // reflash encoder
    vp9_enc.send_eof()?;
    let mut out_pkt = Packet::empty();
    while vp9_enc.receive_packet(&mut out_pkt).is_ok() {
        out_pkt.set_stream(video_stream_idx);
        out_pkt.rescale_ts(
            dec_video.time_base(),
            octx.stream(video_stream_idx).unwrap().time_base(),
        );
        out_pkt.write_interleaved(&mut octx)?;
    }

    octx.write_trailer()?;

    Ok(())
}

pub(crate) fn convert_to_hls(job_id: &str, input: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let temp_vp9_dir = PathBuf::from("/tmp").join(format!("tmp-vp9-{job_id}"));
    std::fs::create_dir_all(&temp_vp9_dir)?;

    // convert to vp9
    convert_to_vp9(input, &temp_vp9_dir)
        .inspect_err(|error| error!(%error, "Failed to convert to vp9"))?;
    let input = temp_vp9_dir;

    // input context
    let input_path = input.to_str().ok_or(anyhow!("Empty input path"))?;
    let mut ictx = format::input(&input_path)?;

    // output context
    let playlist_path = out_dir.join("playlist.m3u8");
    let mut octx = format::output_as(playlist_path.to_str().unwrap(), "hls")?;

    // set HLS options
    let mut opts = Dictionary::new();
    opts.set("hls_time", "4");
    opts.set("hls_playlist_type", "vod");
    opts.set(
        "hls_segment_filename",
        &format!(
            "{}/segment_%03d.ts",
            out_dir.to_str().ok_or(anyhow!("Empty output path"))?
        ),
    );
    octx.set_metadata(opts);

    // codec copy
    for stream in ictx.streams() {
        let codec_id = stream.parameters().id();
        let mut ost = octx.add_stream(codec_id)?;
        ost.set_parameters(stream.parameters());
    }

    octx.write_header()?;

    for (stream, mut packet) in ictx.packets() {
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
