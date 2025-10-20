# Repository Guidelines

## Project Structure & Module Organization
The workspace (`Cargo.toml`) defines a Rust 2024 project with the Axum service in `crates/core`. Its `src/` tree follows the layout described in `CLAUDE.md` (app state, API layer, storage adapters, job system). Token issuance and claim auth live in `crates/claim`, reusable hash utilities in `crates/twox-hash` and `crates/xorf`, and the lightweight harness for end-to-end exercises in `crates/test-server`. Docs live under `docs/`, automation scripts in `scripts/`, and `config.example.toml` documents runtime settings. Check for crate-specific `CLAUDE.md` files when present.

## Build, Test, and Development Commands
Prefer lightweight checks before builds: `cargo fmt --all`, `cargo check`, and `cargo clippy --all-targets --all-features -D warnings`. The Makefile mirrors these (`make fmt`, `make check`, `make test`). Run `cargo nextest run --all-features` (or `make test`) for the canonical test suite; add `-p <crate>` for targeted runs. Use `cargo run --release` to exercise the service, and run `scripts/verify-deps.sh --dry-run` on new machines to ensure FFmpeg libraries are available.

## Coding Style & Naming Conventions
Use nightly `rustfmt` (4-space indentation, snake_case items, CamelCase types) and keep public errors expressive via `thiserror`. Clippy findings are treated as blocking; never silence them with `#[allow(...)]`. Follow the comment guidance in `CLAUDE.md`: document intent, not mechanics, and keep logging consistent with `tracing`. Route all configuration through `Config::load()` so CLI flags, env vars, and TOML remain coherent.

## Testing Guidelines
Integration suites live in `crates/core/tests`; unit tests sit beside their modules (`mod tests`). Prefer `#[tokio::test(flavor = "multi_thread")]` for async paths and reuse `crates/core/test.mp4` to avoid bloated fixtures. Expect up to 10-minute timeouts when running `cargo nextest run --all-features`. For quick iteration, narrow scope with `-p` or `--lib`, but always finish with the full suite before opening a PR, and clean temporary artifacts under `temp/`.

## Commit & Pull Request Guidelines
Follow Conventional Commits (`feat:`, `fix:`, optional scope such as `fix(convert): ...`). Each PR should summarize user-visible impact, enumerate the commands you ran (e.g., `cargo fmt`, `cargo clippy`, `cargo nextest run --all-features`), and link issues. Update `config.example.toml`, `docs/`, or API refs when behavior shifts, and add curl snippets for new endpoints.

## Configuration & Security Tips
Copy `config.example.toml` to `config.toml` for local work, never commit secrets, and prefer MinIO or other test buckets for S3 development. Re-run `scripts/verify-deps.sh --check-only` after OS or toolchain upgrades to confirm FFmpeg compatibility. Track durable job state via `pending-jobs.json`, and clear `uploads/` or `temp/` artifacts that shouldn’t ship.
