# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this directory.

<!--## Crate-specific CLAUDE.md files
Always consult CLAUDE.md files in sub-crates. Instructions in local CLAUDE.md files override instructions
in this file when they are in conflict.-->

## CLAUDE-local.md files
CLAUDE-local.md files are not checked in, and are maintained by individuals. Consult them if they are present.

## Essential Development Commands

### Building and Installation

```bash
# Build a specific crate. Generally don't need to do release build.
cargo build

# Check code without building (preferred)
cargo check
```

### Testing

```bash
# Run Rust unittests
cargo nextest run --all-features # Preferred test runner
```

**Important Notes for Testing:**
- When compiling or running tests in this repository, set timeout limits to at least 10 minutes due to the large codebase size
- For faster iteration, use -p to select only the most relevant packages for testing. Use multiple `-p` flags if necessary, e.g. `cargo nextest run -p sui-types -p sui-core`
- Use `cargo nextest --lib` to run only library tests and skip integration tests for faster feedback
- Consult crate-specific CLAUDE.md files for instructions on which tests to run, when changing files in those crates

### Linting and Formatting

```bash
# Rust
cargo fmt --all -- --check
cargo clippy --all-targets --all-features
```

## High-Level Architecture

### Core Components Structure

```text
video-storage/
├── crates/
│   ├── core/                     # Main application crate
│   │   ├── src/
│   │   │   ├── main.rs           # Application entry point & server initialization
│   │   │   ├── lib.rs            # Main library crate with Axum server setup
│   │   │   ├── app_state.rs      # Shared application state management
│   │   │   ├── config.rs         # Configuration loading & parsing
│   │   │   ├── opendal.rs        # Storage abstraction layer (OpenDAL)
│   │   │   ├── stream_map.rs     # Video streaming utilities
│   │   │   ├── api/              # HTTP API layer
│   │   │   │   ├── routes.rs     # Route handlers for all endpoints
│   │   │   │   └── middleware.rs # HTTP middleware components
│   │   │   └── job/              # Background job processing system
│   │   └── tests/                # Integration tests
│   │   └── Cargo.toml
│   ├── claim/                    # Independent claim management crate
│   │   ├── src/
│   │   │   ├── lib.rs            # Main claim library with validation functions
│   │   │   ├── manager.rs        # Unified ClaimManager (keys + buckets)
│   │   │   ├── bucket.rs         # Token bucket and claim bucket implementation
│   │   │   ├── header.rs         # Claim header with nonce + create_at
│   │   │   ├── payload/          # Claim payload formats
│   │   │   │   ├── mod.rs        # Common payload traits and interfaces
│   │   │   │   ├── payload_v1.rs # v1 payload (single asset)
│   │   │   │   └── payload_v2.rs # v2 payload (multi assets with filter)
│   │   │   ├── middleware.rs     # HTTP middleware for claim auth
│   │   │   ├── create_request.rs # Claim creation API structures
│   │   │   └── error.rs          # Claim error types
│   │   └── Cargo.toml
│   ├── xorf/                     # Binary fuse filters for efficient membership testing
│   ├── twox-hash/                # Fast non-cryptographic hash functions
│   └── test-server/              # Test utilities crate
├── docs/                         # Project documentation
├── scripts/                      # Development & deployment scripts
└── .github/                      # GitHub workflows & templates

```

### Key Architectural Patterns

1.  **Stateful Web Service (Axum)**: The application is a web service built with the Axum framework. It uses a shared, atomically reference-counted `AppState` struct to manage state (configuration, managers, etc.) across all concurrent requests, which is a standard pattern in modern Rust web applications.
2.  **Asynchronous Background Job Queue**: Long-running tasks like video transcoding are handled by a background job system. New jobs are submitted to an `async-channel`, and a worker pool processes them. A `JobSetManager` persists the job queue to disk (`pending-converts.json`) to ensure durability across application restarts. This decouples the API from the heavy lifting.
3.  **Storage Abstraction (OpenDAL)**: The use of the `OpenDAL` library provides a generic interface over different storage backends. This allows the application to be configured to use the local filesystem for development and a cloud provider like AWS S3 for production without changing the core application logic.
4.  **Independent Claim Management System**:
    -   **Modular Design**: The claim system is now a completely independent crate (`video-storage-claim`) that can be used in other projects. This provides better modularity and maintainability.
    -   **Unified ClaimManager**: A single `ClaimManager` handles both cryptographic operations (token signing/verification) and bucket management (rate limiting + concurrency control), eliminating the previous separation between `ClaimManager` and `ClaimBucketManager`.
    -   **Optimized Bucket Keys**: Instead of using full encrypted tokens as bucket keys, the system now uses a combination of nonce (12 bytes) + create_at timestamp (16 bytes) from the claim header, significantly reducing memory usage and CPU overhead.
    -   **Enhanced Header Structure**: Claim headers now include a `create_at` field (128-bit nanosecond timestamp) alongside the existing nonce, providing better uniqueness guarantees for bucket keys.
5.  **Middleware-driven Authorization and Rate Limiting**:
    -   **Claim-Based Auth**: Access to video assets is controlled by the custom claim system. A dedicated middleware intercepts requests to validate time-limited, signed tokens (claims) before allowing access.
    -   **Integrated Bandwidth Throttling**: Token bucket rate limiting is now integrated directly into the claim system, with per-claim bandwidth and concurrency controls applied automatically during video streaming.
6.  **Centralized Configuration**: A single `Config` struct, populated by `clap` and `toml`, centralizes all configuration parameters. This makes the application easy to configure and deploy in different environments.

### Critical Development Notes
1. **Testing Requirements**:
   - Always run tests before submitting changes
   - Framework changes require snapshot updates
2. **CRITICAL - Final Development Steps**:
   - **ALWAYS run `cargo clippy` after finishing development** to ensure code passes all linting checks
   - **NEVER disable or ignore tests** - all tests must pass and be enabled
   - **NEVER use `#[allow(dead_code)]`, `#[allow(unused)]`, or any other linting suppressions** - fix the underlying issues instead
   - **All unit tests must work properly** - use `#[tokio::test]` for async tests, not `#[test]`

### **Comment Writing Guidelines**

**Do NOT comment the obvious** - comments should not simply repeat what the code does.
**When to comment**:
- Non-obvious algorithms or business logic
- Temporary exclusions, timeouts, or thresholds and their reasoning
- Complex calculations where the "why" isn't immediately clear
- Subtle race conditions or threading considerations
- Assumptions about external state or preconditions

**When NOT to comment**:
- Simple variable assignments
- Standard library usage
- Self-descriptive function calls
- Basic control flow (if/for/while)
