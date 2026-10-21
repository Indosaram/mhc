#!/bin/sh
# Install mhc from a GitHub Release.
#
#   curl -fsSL https://raw.githubusercontent.com/Indosaram/mhc/main/install.sh | sh
#
# Layout (install root R = MHC_INSTALL_DIR):
#   R/mhc                  stable launcher; the bin symlink points here and updates never replace it
#   R/current.json         {"schemaVersion":1,"current":"<v>","previous":"<v>"|null}
#   R/versions/<v>/        one complete release package (payload mhc, siblings, skills, manifest.json)
# A flat install from an older release (R/mhc + R/manifest.json, no versions/) is migrated into
# R/versions/<old>/ and kept as `previous`.
#
# Environment:
#   MHC_VERSION      release tag to install (default: latest), e.g. v2026.10.19
#   MHC_INSTALL_DIR  install root (default: ~/.local/share/mhc)
#   MHC_BIN_DIR      where the `mhc` symlink is created (default: ~/.local/bin)
#   MHC_PACKAGE_DIR  install an already extracted release package directory instead of downloading one
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

if [ -z "${MHC_PACKAGE_DIR:-}" ]; then
  command -v curl >/dev/null || die "curl is required"
  command -v tar >/dev/null || die "tar is required"
  if command -v sha256sum >/dev/null; then
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
  elif command -v shasum >/dev/null; then
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
  else
    die "sha256sum or shasum is required"
  fi
fi

# A release version is the string `mhc --version` prints, without a leading v; it becomes a directory name.
valid_version() {
  case "$1" in
    "" | .* | *[!A-Za-z0-9._+-]*) return 1 ;;
  esac
  [ "${#1}" -le 128 ]
}
payload_version() {
  "$1" --version 2>/dev/null | tr -d '\r' | head -n 1 | sed 's/^[vV]//'
}
pointer_field() {
  sed -n "s/.*\"$2\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p" "$1" | head -n 1
}

asset="mhc-$target.tar.gz"
if [ "$version" = latest ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi
local_package="${MHC_PACKAGE_DIR:-}"
[ -z "$local_package" ] || [ -d "$local_package" ] || die "MHC_PACKAGE_DIR is not a directory: $local_package"

mkdir -p "$install_dir" "$bin_dir"
install_dir=$(cd "$install_dir" && pwd -P)
work=$(mktemp -d "$install_dir/.install.XXXXXX")
trap 'rm -rf "$work"' EXIT INT TERM

# Serialize with a running `mhc update` (same flock the updater takes); skipped where flock is absent.
if command -v flock >/dev/null; then
  exec 9>"$install_dir/.update.lock"
  flock -n 9 || die "another mhc update or install is running"
fi

pkg="$work/mhc-$target"
if [ -n "$local_package" ]; then
  cp -R "$local_package" "$pkg"
else
  say "Downloading $asset ($version)..."
  curl -fSL --progress-bar -o "$work/$asset" "$base/$asset" || die "download failed: $base/$asset"
  curl -fsSL -o "$work/$asset.sha256" "$base/$asset.sha256" || die "download failed: $base/$asset.sha256"
  expected=$(cut -d' ' -f1 "$work/$asset.sha256")
  actual=$(sha256 "$work/$asset")
  [ "$expected" = "$actual" ] || die "checksum mismatch for $asset (expected $expected, got $actual)"
  tar -xzf "$work/$asset" -C "$work"
fi
[ -x "$pkg/mhc" ] || die "archive does not contain mhc-$target/mhc"
[ -x "$pkg/mhc-launcher" ] || die "archive does not contain mhc-$target/mhc-launcher"
[ -f "$pkg/manifest.json" ] || die "archive does not contain mhc-$target/manifest.json"

new=$(payload_version "$pkg/mhc")
valid_version "$new" || die "cannot determine the packaged mhc version (got '$new')"

# Pointer state before this install.
old_current=""
old_previous=""
had_pointer=0
if [ -f "$install_dir/current.json" ]; then
  had_pointer=1
  old_current=$(pointer_field "$install_dir/current.json" current)
  old_previous=$(pointer_field "$install_dir/current.json" previous)
  cp "$install_dir/current.json" "$work/current.json.bak"
fi

mkdir -p "$install_dir/versions"

# 1. Publish the package as versions/<new> (an identical existing package is reused).
dest="$install_dir/versions/$new"
if [ -d "$dest" ] && [ -x "$dest/mhc" ] && cmp -s "$dest/manifest.json" "$pkg/manifest.json"; then
  say "mhc $new is already installed; reusing it."
else
  if [ -e "$dest" ]; then
    rm -rf "$install_dir/versions/.replace.$$"
    mv "$dest" "$install_dir/versions/.replace.$$" || die "cannot replace $dest"
    rm -rf "$install_dir/versions/.replace.$$"
  fi
  mv "$pkg" "$dest" || die "cannot install package into $dest"
fi

# 2. Migrate a flat legacy root (payload mhc directly in R, no pointer): every visible top-level entry of the
#    old package moves into versions/<old>.
flat_entries() {
  for entry in "$install_dir"/*; do
    [ -e "$entry" ] || [ -L "$entry" ] || continue
    name=$(basename "$entry")
    case "$name" in versions | current.json) continue ;; esac
    printf '%s\n' "$name"
  done
}
previous="$old_previous"
if [ "$had_pointer" = 1 ]; then
  [ "$old_current" = "$new" ] || previous="$old_current"
elif [ -f "$install_dir/mhc" ] && [ ! -L "$install_dir/mhc" ]; then
  flat=$(payload_version "$install_dir/mhc" || true)
  if valid_version "$flat" && [ "$flat" != "$new" ] && [ ! -e "$install_dir/versions/$flat" ]; then
    say "Migrating the existing flat install ($flat) into versions/$flat..."
    mkdir "$install_dir/versions/$flat"
    flat_entries | while IFS= read -r name; do
      mv "$install_dir/$name" "$install_dir/versions/$flat/$name"
    done
    previous="$flat"
  else
    flat_entries | while IFS= read -r name; do
      rm -rf "${install_dir:?}/$name"
    done
    previous=""
  fi
fi
[ "$previous" = "$new" ] && previous=""

# 3. Stable root launcher (the packaged mhc-launcher), replaced atomically only when it differs.
launcher_src="$install_dir/versions/$new/mhc-launcher"
if [ ! -f "$install_dir/mhc" ] || ! cmp -s "$launcher_src" "$install_dir/mhc"; then
  cp "$launcher_src" "$install_dir/.mhc.new.$$"
  chmod 755 "$install_dir/.mhc.new.$$"
  mv -f "$install_dir/.mhc.new.$$" "$install_dir/mhc"
fi

# 4. Atomically publish current.json.
if [ -n "$previous" ]; then previous_json="\"$previous\""; else previous_json=null; fi
printf '{"schemaVersion":1,"current":"%s","previous":%s}\n' "$new" "$previous_json" > "$install_dir/.current.json.$$"
mv -f "$install_dir/.current.json.$$" "$install_dir/current.json"

ln -sfn "$install_dir/mhc" "$bin_dir/mhc"

installed=$("$install_dir/mhc" --version 2>/dev/null | tr -d '\r' | head -n 1 | sed 's/^[vV]//' || true)
if [ "$installed" != "$new" ]; then
  if [ "$had_pointer" = 1 ]; then cp "$work/current.json.bak" "$install_dir/current.json"; fi
  die "installed launcher reports '$installed', expected '$new'"
fi

say "Installed mhc $installed to $install_dir"
case ":$PATH:" in
  *":$bin_dir:"*) say "Run: mhc" ;;
  *) say "Add $bin_dir to your PATH, e.g.: echo 'export PATH=\"$bin_dir:\$PATH\"' >> ~/.profile" ;;
esac
