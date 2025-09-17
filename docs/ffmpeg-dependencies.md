# FFmpeg Dependencies Guide

This document explains the FFmpeg dependencies required for building the video-storage project.

## Why These Dependencies?

The `ffmpeg-sys-next` crate builds FFmpeg from source during compilation, which requires various development libraries for different codecs and formats. Without these dependencies, the build will fail with errors like "opus not found" or "vpx not found".

## Core FFmpeg Libraries

These are the essential FFmpeg libraries that must be installed:

- `libavcodec-dev`: Core codec library
- `libavformat-dev`: Format (container) library
- `libavutil-dev`: Common utility library
- `libswscale-dev`: Video scaling library
- `libavfilter-dev`: Audio/video filtering library
- `libavdevice-dev`: Device handling library
- `libswresample-dev`: Audio resampling library

## Build Tools

- `nasm`: Assembler for x86 optimizations
- `pkg-config`: Helper tool for compiler/linker flags
- `clang`: C compiler (alternative to gcc)

## Codec Libraries

### Video Codecs

- **libvpx-dev**: VP8/VP9 codec support
  - Required for WebM video encoding/decoding
  - Essential for modern web video compatibility

- **libx264-dev**: H.264/AVC codec support
  - Most widely supported video codec
  - Required for MP4 video encoding

- **libx265-dev**: H.265/HEVC codec support
  - Next-generation video codec
  - Better compression than H.264

### Audio Codecs

- **libopus-dev**: Opus audio codec
  - Modern, high-quality audio codec
  - Required for WebM audio and low-latency streaming

- **libvorbis-dev**: Vorbis audio codec
  - Open-source audio codec
  - Used in OGG containers

- **libmp3lame-dev**: MP3 audio codec
  - Legacy but still widely used audio format

- **libfdk-aac-dev**: AAC audio codec
  - High-quality AAC encoder
  - Common in MP4 containers

### Other Libraries

- **libass-dev**: Subtitle rendering support
  - For burning subtitles into video

## Installation Commands

### Ubuntu/Debian (Complete)

```bash
sudo apt-get update
sudo apt-get install -y \
    # Core FFmpeg libraries
    ffmpeg \
    libavcodec-dev \
    libavformat-dev \
    libavutil-dev \
    libswscale-dev \
    libavfilter-dev \
    libavdevice-dev \
    libswresample-dev \
    # Build tools
    clang \
    pkg-config \
    nasm \
    # Video codecs
    libvpx-dev \
    libx264-dev \
    libx265-dev \
    # Audio codecs
    libopus-dev \
    libvorbis-dev \
    libmp3lame-dev \
    libfdk-aac-dev \
    # Other
    libass-dev
```

### Ubuntu/Debian (Minimal)

If you only need basic functionality:

```bash
sudo apt-get update
sudo apt-get install -y \
    ffmpeg \
    libavcodec-dev \
    libavformat-dev \
    libavutil-dev \
    libswscale-dev \
    libavfilter-dev \
    libavdevice-dev \
    clang \
    pkg-config \
    nasm \
    libopus-dev \
    libvpx-dev
```

### macOS

```bash
# FFmpeg with most codecs included
brew install ffmpeg

# Additional tools
brew install pkg-config nasm

# If you need to compile FFmpeg from source
brew install \
    opus \
    libvpx \
    x264 \
    x265 \
    libvorbis \
    lame \
    fdk-aac
```

### Fedora/RHEL/CentOS

```bash
# Enable RPM Fusion for codec packages
sudo dnf install -y \
    https://download1.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm \
    https://download1.rpmfusion.org/nonfree/fedora/rpmfusion-nonfree-release-$(rpm -E %fedora).noarch.rpm

# Install dependencies
sudo dnf install -y \
    ffmpeg-devel \
    clang \
    pkg-config \
    nasm \
    opus-devel \
    libvpx-devel \
    x264-devel \
    x265-devel \
    libvorbis-devel \
    lame-devel \
    fdk-aac-devel \
    libass-devel
```

### Alpine Linux

```bash
apk add --no-cache \
    ffmpeg-dev \
    clang \
    pkgconfig \
    nasm \
    opus-dev \
    libvpx-dev \
    x264-dev \
    x265-dev \
    libvorbis-dev \
    lame-dev \
    fdk-aac-dev \
    libass-dev
```

## Docker Environment

For a consistent build environment, you can use Docker:

```dockerfile
FROM rust:latest

RUN apt-get update && apt-get install -y \
    libavcodec-dev \
    libavformat-dev \
    libavutil-dev \
    libswscale-dev \
    libavfilter-dev \
    libavdevice-dev \
    libswresample-dev \
    clang \
    pkg-config \
    nasm \
    libopus-dev \
    libvpx-dev \
    libx264-dev \
    libx265-dev \
    libvorbis-dev \
    libmp3lame-dev \
    libfdk-aac-dev \
    libass-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
```

## Troubleshooting

### Missing codec errors

If you see errors like:
- `ERROR: opus not found using pkg-config`
- `ERROR: libvpx not found`

Make sure you have installed the corresponding `-dev` or `-devel` package for that codec.

### pkg-config not finding libraries

Ensure pkg-config is installed and can find the libraries:

```bash
# Check if pkg-config can find a library
pkg-config --exists libavcodec && echo "found" || echo "not found"

# Show the flags for a library
pkg-config --cflags --libs libavcodec
```

### Different package names

Some distributions may have slightly different package names:
- Debian/Ubuntu: `-dev` suffix
- Fedora/RHEL: `-devel` suffix
- Some packages might be in non-free repositories

### Build still failing?

If the build still fails after installing all dependencies:

1. Clean the build cache:
   ```bash
   cargo clean
   ```

2. Check system logs for missing libraries:
   ```bash
   cargo build 2>&1 | grep "not found"
   ```

3. Verify FFmpeg itself can be built:
   ```bash
   # Download and try building FFmpeg manually
   git clone https://github.com/FFmpeg/FFmpeg.git
   cd FFmpeg
   ./configure --enable-libopus --enable-libvpx
   ```

4. Consider using pre-built FFmpeg:
   Some systems allow using system FFmpeg instead of building from source. Check the `ffmpeg-sys-next` documentation for environment variables that control this behavior.
