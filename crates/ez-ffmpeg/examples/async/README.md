# ez-ffmpeg Example: Asynchronous

This example demonstrates how to run FFmpeg operations asynchronously in Rust using the `ez-ffmpeg` crate. By enabling the `async` feature, you can execute the FFmpeg pipeline without blocking the main thread, allowing your application to perform other tasks concurrently while the FFmpeg process is running.

## Key Concepts

1. **Asynchronous Execution**:  
   With the `async` feature enabled, FFmpeg operations are handled asynchronously. This allows for non-blocking execution, so the application can continue other tasks while the FFmpeg pipeline processes the video.

2. **FFmpegContext**:  
   The `FfmpegContext` is built with the input video and output settings. In the provided example, `test.mp4` is processed and saved to `output.mp4`.

3. **Tokio**:  
   This example uses `tokio` to handle asynchronous tasks. The `#[tokio::main]` attribute initializes the asynchronous runtime, enabling the `await` syntax for non-blocking waits.

4. **Starting and Waiting**:  
   The FFmpeg job is started asynchronously with `.start()`, and we `await` the result to ensure the operation completes before the program exits.

## Important Notes

- **Async Feature**:  
  To run FFmpeg operations asynchronously, the `async` feature must be enabled in your `Cargo.toml`. This will allow the use of asynchronous schedulers and prevent blocking the main execution thread.