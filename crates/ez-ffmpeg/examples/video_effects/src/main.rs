use ez_ffmpeg::opengl::opengl_frame_filter::OpenGLFrameFilter;
use ez_ffmpeg::{FfmpegContext, Output};
use ez_ffmpeg::filter::frame_pipeline_builder::FramePipelineBuilder;
use ez_ffmpeg::AVMediaType;

fn main() {
    // Load the fragment shader code from the resource directory
    let fragment_shader = include_str!("../resource/fragment.glsl");

    // Create a frame pipeline builder for video frames (AVMEDIA_TYPE_VIDEO)
    let frame_pipeline_builder: FramePipelineBuilder = AVMediaType::AVMEDIA_TYPE_VIDEO.into();

    // Create an OpenGL-based frame filter using the fragment shader
    let filter = OpenGLFrameFilter::new_simple(fragment_shader.to_string()).unwrap();

    // Attach the OpenGL filter to the frame pipeline builder with a unique name "effect"
    let frame_pipeline_builder = frame_pipeline_builder.filter("effect", Box::new(filter));

    // Build the FFmpeg context, specifying the input file and the output file
    // The output file will apply the frame pipeline with the OpenGL effect filter
    FfmpegContext::builder()
        .input("../../test.mp4") // Input video file
        .output(Output::from("output.mp4").add_frame_pipeline(frame_pipeline_builder)) // Apply frame pipeline to output
        .build().unwrap() // Build the context
        .start().unwrap() // Start the process
        .wait().unwrap(); // Wait for the process to finish
}
