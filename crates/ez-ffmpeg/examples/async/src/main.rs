use ez_ffmpeg::FfmpegContext;

#[tokio::main]
async fn main() {
    // 1. Build FFmpeg context with input and output files
    FfmpegContext::builder()
        .input("../../test.mp4") // Input video file
        .output("output.mp4") // Output video file
        .build().unwrap() // Build FFmpeg context
        .start().unwrap() // Start the FFmpeg job
        .await.unwrap(); // Wait asynchronously for FFmpeg to finish
}
