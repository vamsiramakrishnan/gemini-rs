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

# Normalize once before changing directories so a caller's relative target
# keeps its meaning throughout the generated-project matrix.
project_target=${CARGO_TARGET_DIR:-"$root/target"}
mkdir -p "$project_target"
export CARGO_TARGET_DIR
CARGO_TARGET_DIR=$(cd "$project_target" && pwd)
cargo build -p gemini-adk-cli-rs --locked --manifest-path "$root/Cargo.toml"
adk="$CARGO_TARGET_DIR/debug/adk"
# CLI and generated Rust projects share the same dependency artifacts.
gallery="$root/apps/gemini-adk-web-rs/static/examples/flows"
for spec in "$gallery"/*.json "$gallery"/reference/*.json; do
  [ -f "$spec" ] || continue
  name=$(basename "$spec" .json)
  [ "$name" = index ] && continue
  if [[ "$spec" == "$gallery/reference/"* ]]; then
    name="reference-$name"
  fi
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
# The preserved restaurant workflow exposes a root tool for this protocol smoke.
# Expanded catalogs are exercised through each generated server's scoped tests.
(cd "$out/python/reference-restaurant" && "$adk" spec call agent.json check_availability)
(cd "$out/go/reference-restaurant" && "$adk" spec call agent.json check_availability)
echo "Generated projects: all checks passed ($out)"
