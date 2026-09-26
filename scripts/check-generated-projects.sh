#!/usr/bin/env bash
# Generate a Rust, Python and Go project from every Flow Studio gallery spec
# with `adk spec codegen`, then format-check, lint, build and test each one.
# Finally call a tool through a Python and a Go tool server with
# `adk spec call`, the way a session calls it.
#
# Needs cargo, python3 with `mcp>=2.2,<3` installed, and go >= 1.25.
# Usage: scripts/check-generated-projects.sh [output-dir]
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$(mktemp -d)}
mkdir -p "$out"
out=$(cd "$out" && pwd)

cargo build -p gemini-adk-cli-rs --locked --manifest-path "$root/Cargo.toml"
adk="$root/target/debug/adk"
# Generated Rust projects share one target directory and this repository's
# dependency versions.
export CARGO_TARGET_DIR="$root/target/generated"

for spec in "$root"/apps/gemini-adk-web-rs/static/examples/flows/*.json; do
  name=$(basename "$spec" .json)
  [ "$name" = index ] && continue
  echo "::group::$name"

  "$adk" spec codegen "$spec" --lang rust --out "$out/rust/$name" --sdk-path "$root" --force
  cp "$root/Cargo.lock" "$out/rust/$name/Cargo.lock"
  (cd "$out/rust/$name" &&
    cargo fmt --check &&
    cargo clippy --all-targets -- -D warnings &&
    cargo test)

  "$adk" spec codegen "$spec" --lang python --out "$out/python/$name" --force
  (cd "$out/python/$name" && python3 -m unittest)

  "$adk" spec codegen "$spec" --lang go --out "$out/go/$name" --force
  (cd "$out/go/$name" &&
    go mod tidy &&
    test -z "$(gofmt -l .)" &&
    go vet ./... &&
    go test ./...)

  echo "::endgroup::"
done

# One call through each kind of tool server, as the runtime makes it.
(cd "$out/python/restaurant" && "$adk" spec call agent.json check_availability)
(cd "$out/go/restaurant" && "$adk" spec call agent.json check_availability)
echo "Generated projects: all checks passed ($out)"
