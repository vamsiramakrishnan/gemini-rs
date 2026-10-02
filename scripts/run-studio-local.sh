#!/usr/bin/env bash
# Start the web apps and Flow Studio using the existing repository .env.
set -euo pipefail

studio_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$studio_root"

# Avoid the Conda compiler wrappers and keep build output off the home disk.
export CC=/usr/bin/cc
export CXX=/usr/bin/c++
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/usr/bin/cc
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/tmp/gemini-rs-target}
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
export CARGO_INCREMENTAL=0
export ADK_BUNDLES=${ADK_BUNDLES:-$studio_root/target/studio-bundles}
export ADK_WEB_ADDR=${ADK_WEB_ADDR:-127.0.0.1:25125}

printf 'Web apps: http://%s/\nFlow Studio: http://%s/studio\n' "$ADK_WEB_ADDR" "$ADK_WEB_ADDR"
if [[ -n ${WEB_HOST:-} ]]; then
    printf 'Workstation Studio: https://%s-%s/studio\n' "${ADK_WEB_ADDR##*:}" "$WEB_HOST"
fi
exec cargo run --locked -p gemini-adk-web-rs
