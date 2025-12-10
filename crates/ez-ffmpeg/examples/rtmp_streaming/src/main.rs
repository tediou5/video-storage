use ez_ffmpeg::rtmp::embed_rtmp_server::EmbedRtmpServer;
use ez_ffmpeg::{FfmpegContext, Input, Output};

fn main() {
    // Method 1: Using Embedded RTMP Server (Requires enabling `rtmp` feature)

    // 1. Create and start an embedded RTMP server on "localhost:1935"
    let mut embed_rtmp_server = EmbedRtmpServer::new("localhost:1935");
    let embed_rtmp_server = embed_rtmp_server.start().unwrap();  // Start the RTMP server

    // 2. Create an RTMP "input" stream with app_name="my-app" and stream_key="my-stream"
    //    This returns an `Output` that FFmpeg can push data into.
    let output = embed_rtmp_server
        .create_rtmp_input("my-app", "my-stream")
        .unwrap();

    // 3. Prepare an `Input` pointing to a local file (e.g., "test.mp4")
    let input: Input = Input::from("../../test.mp4")
        .set_readrate(1.0);  // Optional: limit reading speed

    // 4. Build and run the FFmpeg context
    FfmpegContext::builder()
        .input(input)
        .output(output)
        .build().unwrap()
        .start().unwrap()
        .wait().unwrap();

    // Method 2: Using External RTMP Server (No need for `rtmp` feature)

    // 1. Prepare an `Input` pointing to a local file (e.g., "test.mp4")
    let input: Input = Input::from("../../test.mp4")
        .set_readrate(1.0);  // Optional: limit reading speed

    // 2. Output the stream to an external RTMP server
    //    In this case, the stream is pushed to "rtmp://localhost/my-app/my-stream"
    let output = Output::from("rtmp://localhost/my-app/my-stream")
        .set_format("flv")
        .set_video_codec("h264")
        .set_audio_codec("aac")
        .set_format_opt("flvflags", "no_duration_filesize");

    // 3. Build and run the FFmpeg context for external RTMP streaming
    FfmpegContext::builder()
        .input(input)
        .output(output)
        .build().unwrap()
        .start().unwrap()
        .wait().unwrap();
}
