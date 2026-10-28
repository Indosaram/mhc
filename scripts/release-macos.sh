#!/usr/bin/env bash
# Build the macOS release package locally on an Apple Silicon mac, like the CI macos-aarch64 job.
#
# Usage: scripts/release-macos.sh <version> <output-dir>
#   <version>     MHC_BUILD_VERSION, e.g. 2026.10.23
#   <output-dir>  receives mhc-macos-aarch64.tar.gz and mhc-macos-aarch64.tar.gz.sha256
# Needs: cargo, bun, node, git, protoc. Honors CARGO_TARGET_DIR. Never uploads anything.
set -euo pipefail

version="${1:?usage: release-macos.sh <version> <output-dir>}"
outdir="${2:?usage: release-macos.sh <version> <output-dir>}"
version="${version#v}"
test "$(uname -s)-$(uname -m)" = Darwin-arm64
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo"
mkdir -p "$outdir"
outdir="$(cd "$outdir" && pwd)"
target_dir="${CARGO_TARGET_DIR:-$repo/target}"
case "$target_dir" in /*) ;; *) target_dir="$repo/$target_dir" ;; esac
export CARGO_TARGET_DIR="$target_dir"
name=mhc-macos-aarch64
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

export MHC_BUILD_VERSION="$version"
cargo build --release --locked \
  -p maho-cli -p maho-lsp -p maho-senpi-desktop-engine -p maho-git-bash -p maho-ast-grep --bins
rel="$target_dir/release"
test "$("$rel/mhc" --version | tr -d '\r')" = "$MHC_BUILD_VERSION"
test -f "$rel/mhc-launcher"

commit=$(bun -e 'console.log((await Bun.file("assets/skills/manifest.json").json()).browserRuntime.dependency.commit)')
git init -q "$work/omowright"
git -C "$work/omowright" fetch -q --depth 1 https://github.com/code-yeongyu/omowright.git "$commit"
git -C "$work/omowright" checkout -q FETCH_HEAD
(cd "$work/omowright" && bun install --frozen-lockfile --ignore-scripts --production)
node tools/stage-omowright-runtime.mjs --source "$work/omowright" --target "$work/omowright-runtime"

stage="$work/dist/$name"
mkdir -p "$work/dist"
bun tools/package-native.mjs --binary "$rel/mhc" --launcher-binary "$rel/mhc-launcher" --output "$stage" \
  --mhc-lsp-binary "$rel/mhc-lsp" --desktop-engine-binary "$rel/senpi-desktop-engine" \
  --git-bash-binary "$rel/mhc-git-bash" --ast-grep-binary "$rel/mhc-ast-grep" \
  --omowright-runtime "$work/omowright-runtime"
test -x "$stage/senpi-desktop-engine"
test -x "$stage/mhc-launcher"
bun tools/package-native.mjs --verify-only "$stage"
"$stage/mhc" --version

scripts/package-archive.sh "$work/dist" "$name"
mv "$work/dist/$name.tar.gz" "$work/dist/$name.tar.gz.sha256" "$outdir/"
echo "wrote $outdir/$name.tar.gz"
