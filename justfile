# gemini-rs — Development Tasks
# Run `just --list` for all available commands

set dotenv-load

# ─── Setup ───────────────────────────────────────────────────

# Install development dependencies
setup:
    cargo build --workspace
    @echo ""
    @echo "Setup complete. Run 'just test' to verify."
    @echo ""
    @echo "For 'just ci' (the full GitHub Actions mirror):"
    @echo "  cargo install cargo-hack   # per-feature checks"
    @echo "  cargo install cargo-deny   # licences and advisories"
    @echo "Optional: cargo install cargo-watch  (for 'just watch')"

# ─── Quality ─────────────────────────────────────────────────

# This is NOT the full CI matrix — `just ci` is. Use this while working, and
# `just ci` before pushing.
#
# The fast inner loop: formatting, clippy, tests
check: fmt-check lint test
    @echo ""
    @echo "✓ fmt, clippy and tests passed. Run 'just ci' for the full matrix."

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
    RUSTFLAGS="-D warnings" cargo check --workspace --all-targets --locked

# Run clippy lints (includes -D warnings for all targets)
lint:
    RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --locked -- -D warnings

# ─── Testing ─────────────────────────────────────────────────

# Run all workspace tests (with warnings as errors, matches CI)
test:
    RUSTFLAGS="-D warnings" cargo test --workspace --locked

# Run fast lib-only tests (no doc tests, no -D warnings)
test-fast:
    cargo test --workspace --lib

# Run tests for a specific crate (e.g. just test-crate gemini-genai-rs)
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
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --locked

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

# ─── Apps ────────────────────────────────────────────────────

# Run the web UI (mirrors `adk web`)
run-web:
    cargo run -p gemini-adk-web-rs

# Run the REST API server (mirrors `adk api_server`)
run-api:
    cargo run -p gemini-adk-api-rs

# ─── Examples ────────────────────────────────────────────────

# Run the text-chat example
run-text-chat:
    cargo run -p example-text-chat

# Run the voice-chat example
run-voice-chat:
    cargo run -p example-voice-chat

# Run the tool-calling example
run-tool-calling:
    cargo run -p example-tool-calling

# Run the transcription example
run-transcription:
    cargo run -p example-transcription

# ─── Watch Mode ──────────────────────────────────────────────

# Watch for changes and run tests (requires cargo-watch)
watch:
    cargo watch -x "test --workspace --lib"

# Watch for changes and check compilation
watch-check:
    cargo watch -x "check --workspace"

# ─── CI ──────────────────────────────────────────────────────

# `cargo test --workspace` uses default features, so a whole suite can stop
# compiling without it noticing.
#
# Compile every test file, including those behind non-default features
test-all-features:
    cargo check --workspace --all-features --all-targets --locked

# `--no-default-features` is the end that catches a type which only exists
# when a feature is on.
#
# Check both ends of the feature space
features:
    cargo check --workspace --no-default-features --locked
    cargo check --workspace --all-features --locked

# Every feature on its own. Needs `cargo install cargo-hack`.
feature-isolation:
    @command -v cargo-hack >/dev/null 2>&1 || { echo "cargo-hack is not installed. Run: cargo install cargo-hack"; exit 1; }
    cargo hack check --each-feature --no-dev-deps --locked -p gemini-genai-rs -p gemini-adk-rs -p gemini-adk-fluent-rs -p gemini-memory-rs

# Licences and advisories. Needs `cargo install cargo-deny`.
deny:
    @command -v cargo-deny >/dev/null 2>&1 || { echo "cargo-deny is not installed. Run: cargo install cargo-deny"; exit 1; }
    cargo deny check

# The conversation suite the CLI drives.
conversations:
    cargo run -p gemini-adk-cli-rs --locked -- flow ci conversations

# This used to be `fmt-check lint doc-check test` while claiming to match
# Actions exactly — it skipped the feature matrix, cargo-hack, cargo-deny and
# the conversation suite, so a green run here could still be a red push. It is
# slower than `just check` because CI is slower than `just check`.
#
# Every job GitHub Actions runs, in one command
ci: fmt-check lint doc-check test test-all-features features feature-isolation deny conversations
    @echo ""
    @echo "✓ Every job GitHub Actions runs has passed locally."

# ─── Release ─────────────────────────────────────────────────
# Release branch model: just release 0.6.0
#   1. Creates release/v0.6.0 branch from current HEAD
#   2. Auto-formats (cargo fmt) + commits if needed
#   3. Validates (check, clippy, test, cargo publish --dry-run)
#   4. Generates changelog from conventional commits
#   5. Bumps Cargo.toml + Cargo.lock
#   6. Commits "chore(release): v0.6.0" + tags v0.6.0
#   7. Pushes release branch + tag atomically
#   8. Creates PR: release/v0.6.0 → main
#   9. CI takes over: validate → crates.io publish → GitHub Release
#  10. You merge the PR to bring version bump into main

# Release a new version (creates release branch, validates, tags, pushes, opens PR)
release version:
    @bash scripts/release.sh {{version}}

# Dry-run: preview what `just release` would do without any changes
release-dry version:
    @bash scripts/release.sh {{version}} --dry-run

# Preview commits since last tag (changelog preview before release)
release-preview:
    @PREV=$$(git tag --sort=-version:refname | head -1 2>/dev/null || echo ""); \
     if [ -z "$$PREV" ]; then \
       echo "No tags found. All commits:"; git log --oneline HEAD; \
     else \
       echo "Changes since $$PREV:"; \
       git log --oneline --no-decorate "$$PREV..HEAD"; \
       echo ""; \
       echo "Crates: gemini-genai-rs, gemini-adk-rs, gemini-adk-fluent-rs, gemini-adk-server-rs, gemini-adk-cli-rs"; \
       echo "Current version: $$(grep -m1 '^version = ' Cargo.toml | sed 's/.*\"\(.*\)\".*/\1/')"; \
     fi

# Show current version and tag history
release-status:
    @echo "Current version: $$(grep -m1 '^version = ' Cargo.toml | sed 's/.*\"\(.*\)\".*/\1/')"
    @echo ""
    @echo "Tags:"
    @git tag --sort=-version:refname | head -10 2>/dev/null || echo "  (none)"
    @echo ""
    @echo "Release branches:"
    @git branch -a 2>/dev/null | grep "release/" | head -10 || echo "  (none)"
    @echo ""
    @echo "Published crates: gemini-genai-rs, gemini-adk-rs, gemini-adk-fluent-rs, gemini-adk-server-rs, gemini-adk-cli-rs"

# ─── Utilities ───────────────────────────────────────────────

# Show workspace dependency tree
deps:
    cargo tree --workspace --depth 1

# Clean build artifacts
clean:
    cargo clean

# Count lines of code per crate
loc:
    @echo "gemini-genai-rs:" && find crates/gemini-genai-rs/src -name '*.rs' | xargs wc -l | tail -1
    @echo "gemini-adk-rs:" && find crates/gemini-adk-rs/src -name '*.rs' | xargs wc -l | tail -1
    @echo "gemini-adk-fluent-rs:" && find crates/gemini-adk-fluent-rs/src -name '*.rs' | xargs wc -l | tail -1

# Show doc warning counts per crate
doc-warnings:
    @echo "=== gemini-genai-rs ===" && cargo doc --no-deps -p gemini-genai-rs 2>&1 | grep "warning:" | wc -l
    @echo "=== gemini-adk-rs ===" && cargo doc --no-deps -p gemini-adk-rs 2>&1 | grep "warning:" | wc -l
    @echo "=== gemini-adk-fluent-rs ===" && cargo doc --no-deps -p gemini-adk-fluent-rs 2>&1 | grep "warning:" | wc -l

# Show workspace members and test summary
stats:
    @echo "Workspace members:"
    @grep -A 20 '\[workspace\]' Cargo.toml | grep '"' | sed 's/.*"\(.*\)".*/  \1/'
    @echo ""
    @echo "Test count:" && cargo test --workspace --lib 2>&1 | grep "test result" | tail -1
