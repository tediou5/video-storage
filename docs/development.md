# Development Guide

This guide covers setting up your development environment for the video-storage project.

## Prerequisites

### System Dependencies

The project requires FFmpeg development libraries and build tools. Install them based on your operating system.

For detailed information about FFmpeg dependencies and troubleshooting, see [FFmpeg Dependencies Guide](ffmpeg-dependencies.md).

#### Ubuntu/Debian

```bash
sudo apt-get update && sudo apt-get install -y \
    build-essential \
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
    libvpx-dev \
    libx264-dev \
    libx265-dev \
    libvorbis-dev \
    libmp3lame-dev \
    libass-dev \
    libssl-dev
```

**Note**: These codec libraries are essential for FFmpeg to support various video and audio formats:
- `libopus-dev`: Opus audio codec
- `libvpx-dev`: VP8/VP9 video codec (WebM)
- `libx264-dev`: H.264 video codec
- `libx265-dev`: H.265/HEVC video codec
- `libvorbis-dev`: Vorbis audio codec
- `libmp3lame-dev`: MP3 audio codec
- `libass-dev`: Subtitle rendering

#### macOS

```bash
brew install ffmpeg pkg-config nasm
```

**Note**: Homebrew's ffmpeg formula typically includes most codecs by default. If you need specific codecs, you can install ffmpeg with options:
```bash
brew install ffmpeg --with-libvpx --with-opus
```

#### Fedora/RHEL/CentOS

```bash
sudo dnf install -y \
    gcc \
    gcc-c++ \
    make \
    ffmpeg-devel \
    clang \
    nasm \
    opus-devel \
    libvpx-devel \
    x264-devel \
    x265-devel \
    libvorbis-devel \
    lame-devel \
    libass-devel \
    openssl-devel
```

### Rust

This project requires **Rust nightly** due to the use of unstable features and edition 2024.

Install Rust through [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

The project includes a `rust-toolchain.toml` file that pins to a specific nightly version (`nightly-2025-09-15`) to:
- Avoid daily cache invalidation in CI
- Ensure reproducible builds across environments
- Maintain compatibility with unstable features

The toolchain file will automatically use the correct version. If you need to manually install it:

```bash
rustup install nightly
rustup default nightly
```

### Additional Tools

For development, you might want to install:

```bash
# Faster test runner
cargo install cargo-nextest

# Code formatting
rustup component add rustfmt

# Linting
rustup component add clippy
```

### Using Makefile

The project includes a Makefile for common development tasks:

```bash
# Build release binary
make build

# Run tests with nextest
make test

# Run clippy checks
make check

# Format code
make fmt

# Run all (fmt, check, build, test)
make all

# Show all available commands
make help
```

### Using Cargo directly

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Run tests
cargo test

# Run tests with nextest (faster)
cargo nextest run

# Format code
cargo fmt

# Run linter
cargo clippy
```

## Running Locally

### Basic Local Storage

```bash
cargo run -- --listen-on-port 8080 --workspace ./data
```

### With S3/MinIO

First, start MinIO:

```bash
docker run -p 9000:9000 -p 9001:9001 \
  -e MINIO_ROOT_USER=minioadmin \
  -e MINIO_ROOT_PASSWORD=minioadmin \
  minio/minio server /data --console-address ":9001"
```

Then run the service:

```bash
cargo run -- \
  --storage-backend s3 \
  --s3-bucket video-storage \
  --s3-endpoint http://localhost:9000 \
  --s3-region us-east-1 \
  --s3-access-key-id minioadmin \
  --s3-secret-access-key minioadmin
```

## Common Issues

### FFmpeg Build Errors

If you encounter errors like "nasm/yasm not found", make sure you have installed `nasm`:

```bash
# Ubuntu/Debian
sudo apt-get install nasm

# macOS
brew install nasm
```

If you see errors like "opus not found using pkg-config" or similar codec-related errors, ensure all codec libraries are installed:

```bash
# Ubuntu/Debian
sudo apt-get install libopus-dev libvpx-dev libx264-dev libx265-dev

# Fedora/RHEL
sudo dnf install opus-devel libvpx-devel x264-devel x265-devel
```

### Linking Errors

If you get linking errors related to FFmpeg libraries, ensure all FFmpeg development packages are installed and try clearing the build cache:

```bash
cargo clean
cargo build
```

### Permission Errors

Make sure the workspace directory exists and is writable:

```bash
mkdir -p ./data
chmod 755 ./data
```

## Development Workflow

1. **Create a feature branch**
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Make your changes**
   - Write code
   - Add tests
   - Update documentation

3. **Run checks**
   ```bash
   cargo fmt
   cargo clippy -- -D warnings
   cargo test
   ```

4. **Commit and push**
   ```bash
   git add .
   git commit -m "feat: your feature description"
   git push origin feature/your-feature-name
   ```

5. **Create a pull request**
   - Ensure CI passes
   - Request review

## Testing

### Unit Tests

Run all tests:
```bash
cargo test
```

Run specific test:
```bash
cargo test test_name
```

### Integration Tests

Integration tests are in the `tests/` directory:
```bash
cargo test --test '*'
```

### Test Coverage

Generate test coverage report:
```bash
cargo install cargo-tarpaulin
cargo tarpaulin --out Html
```

## Debugging

### Enable debug logging

Set the `RUST_LOG` environment variable:
```bash
RUST_LOG=debug cargo run
```

### Use debugger

With VS Code and rust-analyzer extension:
1. Set breakpoints in the code
2. Press F5 to start debugging

With command line (lldb on macOS, gdb on Linux):
```bash
rust-lldb target/debug/video-storage
```

## Performance Profiling

### Using perf (Linux)

```bash
cargo build --release
perf record --call-graph=dwarf target/release/video-storage
perf report
```

### Using Instruments (macOS)

```bash
cargo build --release
cargo instruments -t "Time Profiler" --bin video-storage
```
