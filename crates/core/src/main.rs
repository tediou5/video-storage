use ffmpeg_next::{self as ffmpeg};
use tracing_subscriber::EnvFilter;
use video_storage_core::Config;

fn init_tracing() {
    // Default to silencing ez_ffmpeg logs unless explicitly overridden via RUST_LOG.
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ez_ffmpeg=off"));

    tracing_subscriber::fmt().with_env_filter(env_filter).init();
}

#[tokio::main]
async fn main() {
    init_tracing();
    ffmpeg::init().unwrap();

    // Load configuration from CLI and/or config file
    let config = Config::load().expect("Failed to load configuration");
    video_storage_core::run(config).await
}
