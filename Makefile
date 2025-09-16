.PHONY: all build test check fmt clean help

# Default target
all: fmt check build test

# Build release binary
build:
	@echo "Building release binary..."
	cargo build --release

# Run tests with nextest (after running checks)
test: check
	@echo "Running tests with nextest..."
	cargo nextest run --all-features

# Run clippy checks
check:
	@echo "Running clippy checks..."
	cargo clippy --all-targets --all-features -- -D warnings

# Format code
fmt:
	@echo "Formatting code..."
	cargo fmt

# Clean build artifacts
clean:
	@echo "Cleaning build artifacts..."
	cargo clean

# Run all checks (format check, clippy, tests)
ci: fmt-check check test

# Check formatting without modifying files
fmt-check:
	@echo "Checking code formatting..."
	cargo fmt -- --check

# Install development dependencies
install-deps:
	@echo "Installing development dependencies..."
	cargo install cargo-nextest
	rustup component add rustfmt clippy

# Run the application
run:
	@echo "Running application..."
	cargo run --release

# Build and run
build-run: build run

# Show help
help:
	@echo "Available targets:"
	@echo "  make build       - Build release binary"
	@echo "  make test        - Run tests with nextest"
	@echo "  make check       - Run clippy checks"
	@echo "  make fmt         - Format code"
	@echo "  make clean       - Clean build artifacts"
	@echo "  make all         - Run fmt, check, build, and test"
	@echo "  make ci          - Run all checks (for CI)"
	@echo "  make run         - Run the application"
	@echo "  make help        - Show this help message"
