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
work="$(mktemp -d "${TMPDIR:-/tmp}/mhc-rel-XXXXXX")"
trap 'rm -rf "$work"' EXIT

omowright_commit() {
  local commit
  commit=$(bun -e 'console.log((await Bun.file("assets/omo-latest/manifest.json").json()).browserRuntime.dependency.commit)' 2>/dev/null | tr -d '\r\n ')
  if ! printf '%s' "$commit" | grep -Eq '^[0-9a-f]{40}$'; then
    echo "could not read browserRuntime.dependency.commit from assets/omo-latest/manifest.json (got '$commit')" >&2
    return 1
  fi
  printf '%s' "$commit"
}

commit=$(omowright_commit)

export MHC_BUILD_VERSION="$version"
cargo build --release --locked \
  -p maho-cli -p maho-ast-grep-mcp -p maho-lsp-daemon -p maho-git-bash-mcp -p maho-senpi-desktop-engine --bins
rel="$target_dir/release"
test "$("$rel/mhc" --version | tr -d '\r')" = "$MHC_BUILD_VERSION"
test -f "$rel/mhc-launcher"

commit=$(omowright_commit)
git init -q "$work/omowright"
git -C "$work/omowright" fetch -q --depth 1 https://github.com/code-yeongyu/omowright.git "$commit"
git -C "$work/omowright" checkout -q FETCH_HEAD
(cd "$work/omowright" && bun install --frozen-lockfile --ignore-scripts --production)
node assets/omo-latest/stage-omowright-runtime.mjs --source "$work/omowright" --target "$work/omowright-runtime"

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

if [ "${MHC_RELEASE_SKIP_MIT:-}" = 1 ]; then
  exit 0
fi

mit_name=mhc-macos-aarch64-mit
MHC_BUILD_VERSION="$version" cargo build --release --locked --no-default-features -p maho-cli --bins
test "$("$target_dir/release/mhc" --version | tr -d '\r')" = "$version"
mit_stage="$work/dist/$mit_name"
bun tools/package-native.mjs --mit --binary "$target_dir/release/mhc" --launcher-binary "$target_dir/release/mhc-launcher" --output "$mit_stage"
bun tools/package-native.mjs --verify-only "$mit_stage"
MHC_MIT_STAGE="$mit_stage" node -e 'const {readFileSync}=require("node:fs");const p=process.env.MHC_MIT_STAGE;if(!p)throw new Error("MHC_MIT_STAGE is not set");const m=JSON.parse(readFileSync(p+"/manifest.json","utf8"));if(m.profile!=="mit")throw new Error("staged manifest is not the mit profile")'
scripts/package-archive.sh "$work/dist" "$mit_name"
mv "$work/dist/$mit_name.tar.gz" "$work/dist/$mit_name.tar.gz.sha256" "$outdir/"
echo "wrote $outdir/$mit_name.tar.gz"
