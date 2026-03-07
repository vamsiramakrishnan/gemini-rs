# gemini-rs — Development Tasks
# Run `just --list` for all available commands

set dotenv-load

# ─── Setup ───────────────────────────────────────────────────

# Install development dependencies
setup:
    cargo build --workspace
    @echo ""
    @echo "Setup complete. Run 'just test' to verify."
    @echo "Optional: cargo install cargo-watch  (for 'just watch')"

# ─── Quality ─────────────────────────────────────────────────

# Run all quality checks (exact CI parity — run before pushing)
check: fmt-check lint test
    @echo ""
    @echo "✓ All checks passed. Safe to push."

# Pre-commit check (alias for 'check')
pre-commit: check

# Format all code
fmt:
    cargo fmt --all

# Check formatting without modifying
fmt-check:
    cargo fmt --all -- --check

# Compile check with -D warnings (catches unused imports, dead code, etc.)
warn-check:
    RUSTFLAGS="-D warnings" cargo check --workspace --all-targets

# Run clippy lints (includes -D warnings for all targets)
lint:
    RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets -- -D warnings

# ─── Testing ─────────────────────────────────────────────────

# Run all workspace tests (with warnings as errors, matches CI)
test:
    RUSTFLAGS="-D warnings" cargo test --workspace

# Run fast lib-only tests (no doc tests, no -D warnings)
test-fast:
    cargo test --workspace --lib

# Run tests for a specific crate (e.g. just test-crate rs-genai)
test-crate crate:
    RUSTFLAGS="-D warnings" cargo test -p {{crate}}

# Run tests with stdout/stderr visible
test-verbose:
    cargo test --workspace -- --nocapture

# ─── Documentation ───────────────────────────────────────────

# Build and open documentation
docs:
    cargo doc --no-deps --workspace --open

# Check docs build with strict warnings (mirrors CI)
doc-check:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

# ─── Build ───────────────────────────────────────────────────

# Build all crates
build:
    cargo build --workspace

# Build in release mode
build-release:
    cargo build --workspace --release

# Check compilation without codegen
check-compile:
    cargo check --workspace

# ─── Cookbooks ───────────────────────────────────────────────

# Run the UI cookbook server (Axum + WebSocket)
run-ui:
    cargo run -p rs-genai-ui

# Run the text-chat cookbook
run-text-chat:
    cargo run -p cookbook-text-chat

# Run the voice-chat cookbook
run-voice-chat:
    cargo run -p cookbook-voice-chat

# Run the tool-calling cookbook
run-tool-calling:
    cargo run -p cookbook-tool-calling

# Run the transcription cookbook
run-transcription:
    cargo run -p cookbook-transcription

# ─── Watch Mode ──────────────────────────────────────────────

# Watch for changes and run tests (requires cargo-watch)
watch:
    cargo watch -x "test --workspace --lib"

# Watch for changes and check compilation
watch-check:
    cargo watch -x "check --workspace"

# ─── CI ──────────────────────────────────────────────────────

# Run the full CI pipeline locally (exact mirror of GitHub Actions)
ci: fmt-check lint doc-check test
    @echo ""
    @echo "✓ CI pipeline passed. Matches GitHub Actions exactly."

# ─── Utilities ───────────────────────────────────────────────

# Show workspace dependency tree
deps:
    cargo tree --workspace --depth 1

# Clean build artifacts
clean:
    cargo clean

# Count lines of code per crate
loc:
    @echo "rs-genai:" && find crates/rs-genai/src -name '*.rs' | xargs wc -l | tail -1
    @echo "rs-adk:" && find crates/rs-adk/src -name '*.rs' | xargs wc -l | tail -1
    @echo "adk-rs-fluent:" && find crates/adk-rs-fluent/src -name '*.rs' | xargs wc -l | tail -1

# Show doc warning counts per crate
doc-warnings:
    @echo "=== rs-genai ===" && cargo doc --no-deps -p rs-genai 2>&1 | grep "warning:" | wc -l
    @echo "=== rs-adk ===" && cargo doc --no-deps -p rs-adk 2>&1 | grep "warning:" | wc -l
    @echo "=== adk-rs-fluent ===" && cargo doc --no-deps -p adk-rs-fluent 2>&1 | grep "warning:" | wc -l

# Show workspace members and test summary
stats:
    @echo "Workspace members:"
    @grep -A 20 '\[workspace\]' Cargo.toml | grep '"' | sed 's/.*"\(.*\)".*/  \1/'
    @echo ""
    @echo "Test count:" && cargo test --workspace --lib 2>&1 | grep "test result" | tail -1
