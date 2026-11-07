#!/usr/bin/env bash
# Clean-build timings for the shapes CI and developers use: a root
# `cargo check`, `cargo check --workspace --all-targets`, and the link-heavy
# `cargo test -p gemini-adk-fluent-rs --no-run`. Each runs from an empty
# target directory and reports seconds, target size and binary count.
#
#   scripts/bench-build.sh <label>      # results in target-bench/<label>.log
#
# No provider key is needed or used: the keys are unset so nothing can reach
# a model by accident.
set -u
unset GEMINI_API_KEY GOOGLE_GENAI_API_KEY GOOGLE_API_KEY GOOGLE_GENAI_USE_VERTEXAI GOOGLE_CLOUD_PROJECT GOOGLE_ACCESS_TOKEN AI_GATEWAY_API_KEY
export CARGO_INCREMENTAL=0
cd "$(dirname "$0")/.."
label=${1:-run}
mkdir -p target-bench
log=target-bench/$label.log
: > "$log"
run() { # name, target dir, command...
  local name=$1 dir=$2; shift 2
  rm -rf "$dir"
  local t0; t0=$(date +%s)
  CARGO_TARGET_DIR=$dir "$@" >/dev/null 2>>"$log"; local rc=$?
  local t1; t1=$(date +%s)
  local bins; bins=$(find "$dir/debug/deps" -maxdepth 1 -type f -perm -u+x ! -name '*.so' ! -name '*.rlib' ! -name '*.rmeta' ! -name '*.d' 2>/dev/null | wc -l)
  echo "$name: $((t1-t0)) s, rc=$rc, target $(du -sh "$dir" | cut -f1), bins $bins" | tee -a "$log"
  rm -rf "$dir"
}
run "check_default_members"       target-bench/m1 cargo check --locked
run "check_workspace_all_targets" target-bench/m2 cargo check --workspace --all-targets --locked
run "test_no_run_fluent"          target-bench/m3 cargo test -p gemini-adk-fluent-rs --no-run --locked
echo "DONE $label" | tee -a "$log"
