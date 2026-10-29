#!/usr/bin/env bash
# Build a release mhc from this checkout and install it as a dated dev build.
# The version reads YYYY.MM.DD-N, where N counts installs made on that date.
# The build is packaged like a release and installed through install.sh as versions/<version>/
# under the install root, with the stable root launcher and current.json.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
install_dir="${MHC_INSTALL_DIR:-$HOME/.local/share/mhc}"
bin_dir="${MHC_BIN_DIR:-$HOME/.local/bin}"
today="$(date +%Y.%m.%d)"

previous=""
if [ -x "$install_dir/mhc" ]; then
  previous="$("$install_dir/mhc" --version 2>/dev/null || true)"
fi
case "$previous" in
  "$today"-*) n="${previous#"$today"-}"; [[ "$n" =~ ^[0-9]+$ ]] || n=0; next=$((n + 1)) ;;
  *) next=1 ;;
esac
version="$today-$next"

cd "$root"
export MHC_BUILD_VERSION="$version"
CARGO_INCREMENTAL=0 cargo build --release -p maho-cli -p maho-senpi-desktop-engine --bins

mkdir -p "$install_dir" "$bin_dir"
work="$(mktemp -d "$install_dir/.dev-install.XXXXXX")"
trap 'rm -rf "$work"' EXIT

commit="$(bun -e 'console.log((await Bun.file("assets/omo-latest/manifest.json").json()).browserRuntime.dependency.commit)')"
git init -q "$work/omowright"
git -C "$work/omowright" fetch -q --depth 1 https://github.com/code-yeongyu/omowright.git "$commit"
git -C "$work/omowright" checkout -q FETCH_HEAD
(cd "$work/omowright" && bun install --frozen-lockfile --ignore-scripts --production)
node assets/omo-latest/stage-omowright-runtime.mjs --source "$work/omowright" --target "$work/omowright-runtime"

bun tools/package-native.mjs --binary target/release/mhc --launcher-binary target/release/mhc-launcher \
  --output "$work/package" --skill-source latest \
  --git-bash-binary target/release/omo-git-bash --desktop-engine-binary target/release/senpi-desktop-engine \
  --omowright-runtime "$work/omowright-runtime"
bun tools/package-native.mjs --verify-only "$work/package"

MHC_PACKAGE_DIR="$work/package" MHC_INSTALL_DIR="$install_dir" MHC_BIN_DIR="$bin_dir" sh "$root/install.sh"

installed="$("$bin_dir/mhc" --version)"
if [ "$installed" != "$version" ]; then
  echo "installed mhc reports '$installed', expected '$version'" >&2
  exit 1
fi
echo "installed mhc $installed"
