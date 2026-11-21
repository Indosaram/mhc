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
# Exits non-zero on the first failing step and prints that step's tail; preflight failures exit 3.
set -euo pipefail

fast=false
for arg in "$@"; do
  case "$arg" in
    --fast) fast=true ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

cd "$(git rev-parse --show-toplevel)"

missing=()
require_tool() {
  local tool=$1
  local install=$2
  local reason=$3
  if ! command -v "$tool" >/dev/null 2>&1; then
    missing+=("missing $tool: install with '$install' ($reason)")
  fi
}

require_tool cargo 'rustup component add cargo' 'required for fmt and clippy'
require_tool rustc 'rustup toolchain install stable' 'required by cargo clippy'
if [[ $fast == false ]]; then
  require_tool protoc 'sudo apt-get install protobuf-compiler' 'crates/maho-ai build.rs compiles protobuf schemas'
  require_tool bun 'curl -fsSL https://bun.sh/install | bash' 'maho-codemode JS fixtures need Bun'
  # The real_program tests resolve via OMO_AST_GREP_SG_PATH, the managed runtime dir, PATH or
  # Homebrew, so accept any of those instead of demanding one command name.
  managed_sg="$HOME/.omo/runtime/ast-grep/$(uname -s | tr 'A-Z' 'a-z')-$(uname -m | sed 's/x86_64/x64/;s/aarch64/arm64/')/sg"
  if ! command -v ast-grep >/dev/null 2>&1 && ! command -v sg >/dev/null 2>&1 \
     && [[ -z ${OMO_AST_GREP_SG_PATH:-} ]] && [[ ! -x $managed_sg ]]; then
    missing+=("missing ast-grep: 'cargo install ast-grep --locked', or set OMO_AST_GREP_SG_PATH (maho-ast-grep real_program tests require the executable)")
  fi
fi
if (( ${#missing[@]} > 0 )); then
  printf '%s\n' "${missing[@]}" >&2
  exit 3
fi

run_step() {
  local label=$1
  shift
  echo "=== $label"
  local log
  log=$(mktemp)
  # The status is captured FIRST: reading `$?` after an `if` whose branch did not run yields 0, which
  # would report a failed suite as `verify-local: OK`.
  local status=0
  "$@" >"$log" 2>&1 || status=$?
  if [[ $status -eq 0 ]]; then
    tail -3 "$log"
    rm -f "$log"
    return 0
  fi
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
