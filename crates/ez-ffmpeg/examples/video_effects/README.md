# ez-ffmpeg Example: Applying Video Effects using OpenGL Shaders

This example demonstrates how to apply custom OpenGL-based video effects using a fragment shader on video frames in the FFmpeg pipeline. It simplifies applying OpenGL shaders to video frames without manually handling the OpenGL context.

## Key Concepts

The main objective of this example is to show how to use the OpenGL filter to apply video effects during video processing. This is done using:

- **OpenGL Fragment Shaders**: These shaders allow you to define custom effects that will be applied to each video frame.
- **Frame Pipeline**: The `FramePipelineBuilder` is used to define the processing steps that video frames will go through before encoding.

### Prerequisites

To use the `OpenGLFrameFilter`, you must enable the `opengl` feature in your `Cargo.toml` file.

```toml
[dependencies.ez-ffmpeg]
version = "x.y.z"
features = ["opengl"]
