#!/usr/bin/env bash
# Verify the current checkout locally. GitHub Actions is disabled for this repository; nothing
# here pushes a branch or starts a run.
#
# Usage: scripts/verify-local.sh [--fast]
#   --fast   clippy + fmt only, skipping the test suite.
#
# Runs, in order, on the checkout as it stands (staged, unstaged and untracked files included):
#   cargo fmt --all --check
#   cargo clippy --workspace --all-targets -- -D warnings
#   cargo nextest run --workspace   (unless --fast)
#
# Exits non-zero on the first failing step and prints that step's tail.
set -euo pipefail

fast=false
for arg in "$@"; do
  case "$arg" in
    --fast) fast=true ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

cd "$(git rev-parse --show-toplevel)"

run_step() {
  local label=$1
  shift
  echo "=== $label"
  local log
  log=$(mktemp)
  if "$@" >"$log" 2>&1; then
    tail -3 "$log"
    rm -f "$log"
    return 0
  fi
  local status=$?
  echo "FAILED: $label" >&2
  tail -40 "$log" >&2
  rm -f "$log"
  return "$status"
}

run_step "fmt" cargo fmt --all --check
run_step "clippy" cargo clippy --workspace --all-targets --message-format short -- -D warnings

if [[ $fast == false ]]; then
  run_step "nextest" cargo nextest run --workspace
fi

echo "verify-local: OK"
