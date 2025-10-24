# video storage

## System Dependencies Installation

### Ubuntu 22.04

Install FFmpeg and development libraries:

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential \
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

### Rocky Linux 9

Install FFmpeg and development libraries:

```bash
# Enable EPEL and CRB repositories
sudo dnf install -y epel-release
sudo dnf config-manager --set-enabled crb

# Enable RPM Fusion for FFmpeg
sudo dnf install -y --nogpgcheck \
  https://mirrors.rpmfusion.org/free/el/rpmfusion-free-release-9.noarch.rpm \
  https://mirrors.rpmfusion.org/nonfree/el/rpmfusion-nonfree-release-9.noarch.rpm

# Install FFmpeg and development packages
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

### Installing Dependencies Automatically

Use the provided script to automatically install all required dependencies:

```bash
# Automatically install all missing dependencies
sudo ./scripts/verify-deps.sh

# Check what would be installed without actually installing
./scripts/verify-deps.sh --dry-run

# Only check if dependencies are installed (no installation)
./scripts/verify-deps.sh --check-only

# Force reinstall all dependencies
sudo ./scripts/verify-deps.sh --force

# Show detailed output during installation
sudo ./scripts/verify-deps.sh --verbose
```

The script will:
- ✅ Automatically detect your operating system (Ubuntu 22.04 or Rocky Linux 9)
- ✅ Check which dependencies are already installed
- ✅ Install only the missing dependencies
- ✅ Verify the installation was successful
- ✅ Test compilation if Rust is available

Options:
- `--help`: Show usage information
- `--dry-run`: Show what would be installed without making changes
- `--check-only`: Only check dependencies, don't install
- `--force`: Force reinstall all dependencies
- `--verbose`: Show detailed output
- `--version`: Show script version

Exit codes:
- 0: All dependencies successfully installed or already present
- 1: Some dependencies failed to install

## Requirements

- Rust nightly (required for unstable features and edition 2024)
- FFmpeg development libraries
- See [Development Guide](docs/development.md) for detailed setup instructions

## Development

For development setup and build instructions, see [Development Guide](docs/development.md).

## Cli

```text
Usage: video-storage [OPTIONS]

Options:
  -l, --listen-on-port <LISTEN_ON_PORT>
          Port to external API listen on [default: 32145]
      --internal-port <INTERNAL_PORT>
          Internal API port to listen on [default: 32146]
  -p, --permits <PERMITS>
          Number of concurrent conversion jobs [default: 5]
  -t, --token-rate <TOKEN_RATE>
          Token bucket rate limiting (0.0 = disabled) [default: 0]
  -w, --workspace <WORKSPACE>
          Working directory for file storage [default: .]
  -c, --config <CONFIG>
          Configuration file path (overrides all other arguments)
  -s, --storage-backend <STORAGE_BACKEND>
          Storage backend: local or s3 [default: local]
      --s3-bucket <S3_BUCKET>
          S3 bucket name (required when storage-backend is s3)
      --s3-endpoint <S3_ENDPOINT>
          S3 endpoint (for MinIO/custom S3)
      --s3-region <S3_REGION>
          S3 region
      --s3-access-key-id <S3_ACCESS_KEY_ID>
          S3 access key ID
      --s3-secret-access-key <S3_SECRET_ACCESS_KEY>
          S3 secret access key
      --webhook-url <WEBHOOK_URL>
          Webhook URL to call when jobs complete
      --claim-key <CLAIM_KEYS>
          Claim signing keys configuration (kid -> base64 encoded 32-byte key). Can be specified multiple times as --claim-key 1:base64key. You can generate a key with: openssl rand -base64 32
  -h, --help
          Print help
  -V, --version
          Print version
```

## Examples

### Building from source

```shell
# Using Makefile (recommended)
make build        # Build release binary
make test         # Run tests with nextest
make check        # Run clippy checks
make all          # Run fmt, check, build, and test

# Using Cargo directly
# The project uses rust-toolchain.toml to automatically select nightly
cargo build --release
```

### Using command line arguments

```shell
video-storage -p 1 \
  -s s3 \
  --s3-bucket video-storage \
  --s3-endpoint http://127.0.0.1:9000 \
  --s3-region us-east-1 \
  --s3-access-key-id minioadmin \
  --s3-secret-access-key minioadmin \
  --webhook-url https://example.com/webhook
```

### Using configuration file

```shell
video-storage -c config.toml
```

Example config.toml:

```toml
# Server configuration
listen_on_port = 32145
internal_port = 32146
permits = 10
# Minimum permits is 10 to accommodate codec/scale concurrency and uploads
token_rate = 0.0
workspace = "./data"

# Claim signing keys configuration (optional)
# If not specified, keys will be randomly generated on startup
# Format: kid = "base64_encoded_32_byte_key"
# You can generate a key with: openssl rand -base64 32
# [claim_keys]
# 1 = "IaNHoHtWetGMPkHj6Iy8MZe5L3KlH8F6j6nRvJpYQYU="
# 2 = "uBhfVeH0b7KQKfwOJqhwzLXKBpg7xLPBe5HjCksDDWg="

# Storage configuration
storage_backend = "s3"  # Options: "local" or "s3"

# S3 configuration (required when storage_backend = "s3")
s3_bucket = "video-storage"
s3_endpoint = "http://127.0.0.1:9000"
s3_region = "us-east-1"
s3_access_key_id = "minioadmin"
s3_secret_access_key = "minioadmin"

# Webhook configuration (optional)
webhook_url = "https://example.com/webhook"
```

## Webhook

When configured with `--webhook-url` or in config file, the service will send a POST request to the webhook URL when jobs complete.

Webhook payload format:

```json
{
  "job_id": "video",
  "job_type": "convert",  // or "upload"
  "status": "completed",
  "timestamp": "2025-01-09T12:34:56Z"
}
```

## Check waitlist

Returns JSON with pending job counts.

example:

```shell
curl -X GET http://127.0.0.1:32145/waitlist
```

response:

```json
{
  "pending_convert_jobs": 2,
  "pending_upload_jobs": 1,
  "total_pending_jobs": 3
}
```

## Upload video

```shell
curl -X POST "http://0.0.0.0:32145/upload?id=video&crf=48" --data-binary @video.mp4 -H "Content-Type: application/octet-stream"
```

- id: not contain ' ', '-', '/', '.' and not exist
- crf: between 0 and 63

## Get resource

```shell
http://127.0.0.1:32145/videos/video.m3u8
```

## Authentication and Authorization

The service supports claim-based authentication for protected resources. Claims can restrict access based on:

- Asset ID
- Time window (nbf and exp timestamps)
- Playback window length (for HLS segments)
- Maximum bandwidth (kbps)
- Maximum concurrency
- Allowed video widths/resolutions

### Creating a claim

```shell
# Minimal claim (all limits optional, defaults to no restrictions)
curl -X POST http://127.0.0.1:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "video123",
    "exp_unix": 1735689600
  }'

# Claim with optional restrictions
curl -X POST http://127.0.0.1:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "video123",
    "exp_unix": 1735689600,
    "nbf_unix": 1735603200,
    "window_len_sec": 300,
    "max_kbps": 4000,
    "max_concurrency": 10,
    "allowed_widths": [1920, 1280, 854]
  }'
```

Parameters:
- `asset_id`: The ID of the video asset to grant access to (required)
- `exp_unix`: Expiration timestamp (Unix time) (required)
- `nbf_unix`: Not-before timestamp (Unix time, optional, defaults to current time)
- `window_len_sec`: Playback window duration in seconds (optional, 0 = no limit)
- `max_kbps`: Maximum bandwidth limit in kilobits per second (optional, 0 = no limit)
- `max_concurrency`: Maximum concurrent connections (optional, 0 = no limit)
- `allowed_widths`: Array of allowed video widths (resolutions) (optional, empty = all widths allowed)

### Using a claim

Include the claim token in the Authorization header:

```shell
curl -X GET http://127.0.0.1:32145/videos/1920/video123.m3u8 \
  -H "Authorization: Bearer YOUR_CLAIM_TOKEN"
```

The service will validate:
1. The token is valid and not expired
2. The requested asset matches the claim's asset_id
3. The requested width (1920 in this example) is in the allowed_widths list
4. The request is within bandwidth and concurrency limits
