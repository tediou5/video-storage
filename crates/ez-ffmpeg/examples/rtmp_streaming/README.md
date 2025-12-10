# # ez-ffmpeg Example: RTMP Streaming

This example demonstrates two methods for streaming video using **RTMP**:

1. **Using an Embedded RTMP Server**  
2. **Using an External RTMP Server**

## Method 1: Using an Embedded RTMP Server

The `EmbedRtmpServer` allows you to create an RTMP server directly in memory. This method is useful for local or test streaming scenarios, and it bypasses the need for actual network sockets.

### Prerequisites
- To use the **Embedded RTMP Server**, the `rtmp` feature must be enabled in your `Cargo.toml`.

### Steps:
1. **Start the RTMP server**:  
   Create and start an embedded RTMP server running on `localhost:1935` (or your desired address/port).

2. **Create the RTMP input stream**:  
   You will create an input stream with a specific app name and stream key (e.g., `my-app` and `my-stream`) that the server can accept.

3. **Prepare the input source**:  
   Specify the video file you want to stream (e.g., `test.mp4`), and optionally set the reading rate for the input file.

4. **Run FFmpeg to stream to the RTMP server**:  
   FFmpeg will push the data from the input file to the embedded RTMP server you just created.

### Feature Flag:
To enable the embedded RTMP server, you need to include the `rtmp` feature in your `Cargo.toml` file like this:

```toml
[dependencies]
ez-ffmpeg = { version = "X.Y.Z", features = ["rtmp"] }
```

## Method 2: Using an External RTMP Server

If you don’t need an embedded RTMP server and instead want to stream to a real, external RTMP server (e.g., YouTube, Twitch, or any other RTMP destination), you can use this method.

### Steps:
1. **Prepare the input source**:  
   Just like in the first method, specify the video file you want to stream (e.g., `test.mp4`).

2. **Set the external RTMP server URL**:  
   Set the output stream to an external RTMP server URL (e.g., `rtmp://localhost/my-app/my-stream`).

3. **Run FFmpeg to stream to the external RTMP server**:  
   FFmpeg will push the data from the input file to the specified external RTMP server.

### Feature Flag:
No special feature flag is required to use an external RTMP server.

## Conclusion

- **Embedded RTMP Server**: Ideal for local streaming or testing purposes. Requires the `rtmp` feature.
- **External RTMP Server**: Stream to any remote RTMP server like YouTube, Twitch, etc. No need for the `rtmp` feature.
```
