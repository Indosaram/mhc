#!/usr/bin/env bash
# Build the Linux release package locally, like the CI linux-x86_64 job (glibc 2.28 floor).
#
# Usage: scripts/release-linux.sh <version> <output-dir>
#   <version>     MHC_BUILD_VERSION, e.g. 2026.10.23
#   <output-dir>  receives mhc-linux-x86_64.tar.gz and mhc-linux-x86_64.tar.gz.sha256
# Needs: cargo, cargo-zigbuild + zig (pip3 install ziglang cargo-zigbuild), bun, node, git.
# Honors CARGO_TARGET_DIR. Never uploads anything.
set -euo pipefail

version="${1:?usage: release-linux.sh <version> <output-dir>}"
outdir="${2:?usage: release-linux.sh <version> <output-dir>}"
version="${version#v}"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo"
mkdir -p "$outdir"
outdir="$(cd "$outdir" && pwd)"
target_dir="${CARGO_TARGET_DIR:-$repo/target}"
case "$target_dir" in /*) ;; *) target_dir="$repo/$target_dir" ;; esac
export CARGO_TARGET_DIR="$target_dir"
name=mhc-linux-x86_64
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

export MHC_BUILD_VERSION="$version"
rustup target add x86_64-unknown-linux-gnu >/dev/null 2>&1 || true
cargo zigbuild --release --locked --target x86_64-unknown-linux-gnu.2.28 \
  -p maho-cli -p maho-ast-grep-mcp -p maho-lsp-daemon -p maho-git-bash-mcp -p maho-senpi-desktop-engine --bins
rel="$target_dir/x86_64-unknown-linux-gnu/release"
test "$("$rel/mhc" --version | tr -d '\r')" = "$MHC_BUILD_VERSION"
test -f "$rel/mhc-launcher"

# materialize browser skill runtime
commit=$(bun -e 'console.log((await Bun.file("assets/omo-latest/manifest.json").json()).browserRuntime.dependency.commit)')
git init -q "$work/omowright"
git -C "$work/omowright" fetch -q --depth 1 https://github.com/code-yeongyu/omowright.git "$commit"
git -C "$work/omowright" checkout -q FETCH_HEAD
(cd "$work/omowright" && bun install --frozen-lockfile --ignore-scripts --production)
node assets/omo-latest/stage-omowright-runtime.mjs --source "$work/omowright" --target "$work/omowright-runtime"

# package
stage="$work/dist/$name"
mkdir -p "$work/dist"
bun tools/package-native.mjs --binary "$rel/mhc" --launcher-binary "$rel/mhc-launcher" --output "$stage" --skill-source latest \
  --git-bash-binary "$rel/omo-git-bash" --desktop-engine-binary "$rel/senpi-desktop-engine" \
  --omowright-runtime "$work/omowright-runtime"
test -x "$stage/senpi-desktop-engine"
test -x "$stage/mhc-launcher"
bun tools/package-native.mjs --verify-only "$stage"
"$stage/mhc" --version

scripts/package-archive.sh "$work/dist" "$name"
mv "$work/dist/$name.tar.gz" "$work/dist/$name.tar.gz.sha256" "$outdir/"
echo "wrote $outdir/$name.tar.gz"
