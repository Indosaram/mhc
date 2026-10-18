#!/bin/sh
# Install mhc from a GitHub Release.
#
#   curl -fsSL https://raw.githubusercontent.com/Indosaram/mhc/main/install.sh | sh
#
# Environment:
#   MHC_VERSION      release tag to install (default: latest), e.g. v2026.10.19
#   MHC_INSTALL_DIR  where the release directory is unpacked (default: ~/.local/share/mhc)
#   MHC_BIN_DIR      where the `mhc` symlink is created (default: ~/.local/bin)
set -eu

repo="Indosaram/mhc"
version="${MHC_VERSION:-latest}"
install_dir="${MHC_INSTALL_DIR:-$HOME/.local/share/mhc}"
bin_dir="${MHC_BIN_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'mhc install: %s\n' "$*" >&2; exit 1; }

case "$(uname -s)/$(uname -m)" in
  Linux/x86_64 | Linux/amd64) target="linux-x86_64" ;;
  Darwin/arm64 | Darwin/aarch64) target="macos-aarch64" ;;
  *) die "no prebuilt mhc for $(uname -s) $(uname -m); build from source: https://github.com/$repo#build-from-source" ;;
esac

command -v curl >/dev/null || die "curl is required"
command -v tar >/dev/null || die "tar is required"
if command -v sha256sum >/dev/null; then
  sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null; then
  sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  die "sha256sum or shasum is required"
fi

asset="mhc-$target.tar.gz"
if [ "$version" = latest ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi

mkdir -p "$install_dir" "$bin_dir"
work=$(mktemp -d "$install_dir/.install.XXXXXX")
trap 'rm -rf "$work"' EXIT INT TERM

say "Downloading $asset ($version)..."
curl -fSL --progress-bar -o "$work/$asset" "$base/$asset" || die "download failed: $base/$asset"
curl -fsSL -o "$work/$asset.sha256" "$base/$asset.sha256" || die "download failed: $base/$asset.sha256"
expected=$(cut -d' ' -f1 "$work/$asset.sha256")
actual=$(sha256 "$work/$asset")
[ "$expected" = "$actual" ] || die "checksum mismatch for $asset (expected $expected, got $actual)"

tar -xzf "$work/$asset" -C "$work"
[ -x "$work/mhc-$target/mhc" ] || die "archive does not contain mhc-$target/mhc"

for entry in "$work/mhc-$target"/* "$work/mhc-$target"/.[!.]*; do
  [ -e "$entry" ] || continue
  name=$(basename "$entry")
  rm -rf "${install_dir:?}/$name"
  mv "$entry" "$install_dir/$name"
done
ln -sfn "$install_dir/mhc" "$bin_dir/mhc"

say "Installed mhc $("$bin_dir/mhc" --version) to $install_dir"
case ":$PATH:" in
  *":$bin_dir:"*) say "Run: mhc" ;;
  *) say "Add $bin_dir to your PATH, e.g.: echo 'export PATH=\"$bin_dir:\$PATH\"' >> ~/.profile" ;;
esac
