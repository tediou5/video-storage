use ffmpeg_next::{self as ffmpeg};
use video_storage::Config;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    ffmpeg::init().unwrap();

    // Load configuration from CLI and/or config file
    let config = Config::load().expect("Failed to load configuration");
    video_storage::run(config).await
}
