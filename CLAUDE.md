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
├── src/                      # Main Rust source code
│   ├── main.rs              # Application entry point & server initialization
│   ├── lib.rs               # Main library crate with Axum server setup
│   ├── app_state.rs         # Shared application state management
│   ├── config.rs            # Configuration loading & parsing
│   ├── opendal.rs           # Storage abstraction layer (OpenDAL)
│   ├── stream_map.rs        # Video streaming utilities
│   ├── api/                 # HTTP API layer
│   │   ├── routes.rs        # Route handlers for all endpoints
│   │   ├── middleware.rs    # HTTP middleware components
│   │   └── token_bucket.rs  # Rate limiting for video streaming
│   ├── claim/               # Access control & authorization
│   └── job/                 # Background job processing system
├── tests/                   # Integration tests
├── docs/                    # Project documentation
├── scripts/                 # Development & deployment scripts
└── .github/                 # GitHub workflows & templates

```

### Key Architectural Patterns

1.  **Stateful Web Service (Axum)**: The application is a web service built with the Axum framework. It uses a shared, atomically reference-counted `AppState` struct to manage state (configuration, managers, etc.) across all concurrent requests, which is a standard pattern in modern Rust web applications.
2.  **Asynchronous Background Job Queue**: Long-running tasks like video transcoding are handled by a background job system. New jobs are submitted to an `async-channel`, and a worker pool processes them. A `JobSetManager` persists the job queue to disk (`pending-converts.json`) to ensure durability across application restarts. This decouples the API from the heavy lifting.
3.  **Storage Abstraction (OpenDAL)**: The use of the `OpenDAL` library provides a generic interface over different storage backends. This allows the application to be configured to use the local filesystem for development and a cloud provider like AWS S3 for production without changing the core application logic.
4.  **Middleware-driven Authorization and Rate Limiting**:
    -   **Claim-Based Auth**: Access to video assets is controlled by a custom claim system. A dedicated middleware (`claim_middleware.rs`) likely intercepts requests to validate time-limited, signed tokens (claims) before allowing access.
    -   **Bandwidth Throttling**: A `TokenBucket` implementation is used to enforce rate limits on video streaming, preventing abuse and managing resource allocation. This is applied when serving video file chunks.
5.  **Centralized Configuration**: A single `Config` struct, populated by `clap` and `toml`, centralizes all configuration parameters. This makes the application easy to configure and deploy in different environments.

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
