#!/usr/bin/env bash
# Build a release mhc from this checkout and install it as a dated dev build.
# The version reads YYYY.MM.DD-N, where N counts installs made on that date.
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
MHC_BUILD_VERSION="$version" CARGO_INCREMENTAL=0 cargo build --release -p maho-cli --bin mhc

mkdir -p "$install_dir" "$bin_dir"
install -m 755 target/release/mhc "$install_dir/mhc.new"
mv "$install_dir/mhc.new" "$install_dir/mhc"
ln -sfn "$install_dir/mhc" "$bin_dir/mhc"

installed="$("$bin_dir/mhc" --version)"
if [ "$installed" != "$version" ]; then
  echo "installed mhc reports '$installed', expected '$version'" >&2
  exit 1
fi
echo "installed mhc $installed"
