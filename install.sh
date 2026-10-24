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
#   MHC_INSTALL_BASE_URL  download assets from this URL instead of the GitHub release (testing)
#   MHC_ALLOW_UNSIGNED=1  allow a release older than the first signed version that has no .minisig
#   MHC_VERIFY_ATTESTATION=1  also run `gh attestation verify` when gh is installed
set -eu

repo="Indosaram/mhc"
# Trusted release signing key (same value as mhc-release.pub); never fetched from the release.
MHC_RELEASE_PUBKEY='RWR4GANxRiLz3QbBlgDTEW7J++qZ1fVmYPhYEN7cAnSsW4IJJ/smaLju'
# Releases from this version on must carry a .minisig.
first_signed_version="2026.10.24.1"
version="${MHC_VERSION:-latest}"
install_dir="${MHC_INSTALL_DIR:-$HOME/.local/share/mhc}"
bin_dir="${MHC_BIN_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'mhc install: %s\n' "$*" >&2; exit 1; }

case "$(uname -s)/$(uname -m)" in
  Linux/x86_64 | Linux/amd64) target="linux-x86_64" ;;
  Linux/aarch64 | Linux/arm64) target="linux-aarch64" ;;
  Darwin/arm64 | Darwin/aarch64) target="macos-aarch64" ;;
  Darwin/x86_64) target="macos-x86_64" ;;
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
# Prints the version; the binary's stdout, stderr and exit status are kept under $work for probe_report.
payload_version() {
  echo 0 >"$work/probe.status"
  "$1" --version >"$work/probe.out" 2>"$work/probe.err" || echo "$?" >"$work/probe.status"
  tr -d '\r' <"$work/probe.out" | head -n 1 | sed 's/^[vV]//'
}
# Why the last probe failed: exit status, first stderr lines, and a hint for a too-old system glibc.
probe_report() {
  printf ' (exit status %s)' "$(cat "$work/probe.status")"
  if [ -s "$work/probe.err" ]; then
    printf '\n'
    head -n 5 "$work/probe.err" | tr -d '\r' | sed 's/^/  /'
    if grep -q 'GLIBC_' "$work/probe.err"; then
      printf 'This mhc build needs a newer glibc than this system provides; please report it at https://github.com/Indosaram/mhc/issues'
    fi
  fi
}
pointer_field() {
  sed -n "s/.*\"$2\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p" "$1" | head -n 1
}

# True when dotted numeric version $1 is >= $2; false when either is not purely numeric dotted.
version_ge() {
  case "$1$2" in *[!0-9.]* | "") return 1 ;; esac
  a="$1"
  b="$2"
  while [ -n "$a$b" ]; do
    ah="${a%%.*}"
    bh="${b%%.*}"
    [ -n "$ah" ] || ah=0
    [ -n "$bh" ] || bh=0
    [ "$ah" -gt "$bh" ] && return 0
    [ "$ah" -lt "$bh" ] && return 1
    case "$a" in *.*) a="${a#*.}" ;; *) a="" ;; esac
    case "$b" in *.*) b="${b#*.}" ;; *) b="" ;; esac
  done
  return 0
}
# Verifies $work/$asset against the pinned key. The release-hosted key is never used.
verify_signature() {
  tag="$version"
  if [ "$tag" = latest ]; then
    tag=""
    case "$final_url" in
      */releases/download/*/"$asset") tag="${final_url%/*}"; tag="${tag##*/}" ;;
    esac
  fi
  if ! curl -fsSL -o "$work/$asset.minisig" "$base/$asset.minisig" 2>/dev/null; then
    rm -f "$work/$asset.minisig"
    # An unresolvable tag is treated as current, i.e. signature required.
    if [ -z "$tag" ] || version_ge "${tag#v}" "$first_signed_version"; then
      die "release ${tag:-latest} has no signature ($asset.minisig); refusing to install"
    fi
    [ "${MHC_ALLOW_UNSIGNED:-}" = 1 ] || die "release $tag is unsigned; set MHC_ALLOW_UNSIGNED=1 to install it anyway"
    say "Warning: $tag is unsigned; installing because MHC_ALLOW_UNSIGNED=1 (sha256 verified)."
    return 0
  fi
  if command -v minisign >/dev/null; then
    minisign -Vm "$work/$asset" -P "$MHC_RELEASE_PUBKEY" >/dev/null || die "signature verification failed for $asset"
    say "Signature verified."
  else
    say "minisign not found; skipped signature check (sha256 verified). Install minisign for full verification."
  fi
  if [ "${MHC_VERIFY_ATTESTATION:-}" = 1 ] && command -v gh >/dev/null; then
    gh attestation verify "$work/$asset" --repo "$repo" || die "attestation verification failed for $asset"
  fi
}

asset="mhc-$target.tar.gz"
if [ -n "${MHC_INSTALL_BASE_URL:-}" ]; then
  base="${MHC_INSTALL_BASE_URL%/}"
elif [ "$version" = latest ]; then
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
  final_url=$(curl -fSL --progress-bar -o "$work/$asset" -w '%{url_effective}' "$base/$asset") || die "download failed: $base/$asset"
  curl -fsSL -o "$work/$asset.sha256" "$base/$asset.sha256" || die "download failed: $base/$asset.sha256"
  expected=$(cut -d' ' -f1 "$work/$asset.sha256")
  actual=$(sha256 "$work/$asset")
  [ "$expected" = "$actual" ] || die "checksum mismatch for $asset (expected $expected, got $actual)"
  verify_signature
  tar -xzf "$work/$asset" -C "$work"
fi
[ -x "$pkg/mhc" ] || die "archive does not contain mhc-$target/mhc"
[ -x "$pkg/mhc-launcher" ] || die "archive does not contain mhc-$target/mhc-launcher"
[ -f "$pkg/manifest.json" ] || die "archive does not contain mhc-$target/manifest.json"

new=$(payload_version "$pkg/mhc")
valid_version "$new" || die "cannot determine the packaged mhc version (got '$new')$(probe_report)"

# Pointer state before this install.
old_current=""
old_previous=""
had_pointer=0
if [ -f "$install_dir/current.json" ]; then
  had_pointer=1
  old_current=$(pointer_field "$install_dir/current.json" current)
  old_previous=$(pointer_field "$install_dir/current.json" previous)
  valid_version "$old_current" || die "invalid current version pointer"
  [ -z "$old_previous" ] || valid_version "$old_previous" || die "invalid previous version pointer"
  cp "$install_dir/current.json" "$work/current.json.bak"
fi

mkdir -p "$install_dir/versions"

# 1. Publish the package as versions/<new> (an identical existing package is reused).
dest="$install_dir/versions/$new"
if [ -d "$dest" ] && [ -x "$dest/mhc" ] && cmp -s "$dest/manifest.json" "$pkg/manifest.json"; then
  say "mhc $new is already installed; reusing it."
else
  [ ! -e "$dest" ] || die "version $new already exists with different content; published packages are immutable"
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
    die "cannot safely migrate the existing flat installation; use a separate MHC_INSTALL_DIR"
  fi
fi
[ "$previous" = "$new" ] && previous=""

# 3. Bootstrap the stable launcher; an existing versioned installation keeps its launcher.
launcher_src="$install_dir/versions/$new/mhc-launcher"
if [ "$had_pointer" = 0 ] || [ ! -f "$install_dir/mhc" ]; then
  cp "$launcher_src" "$install_dir/.mhc.new.$$"
  chmod 755 "$install_dir/.mhc.new.$$"
  mv -f "$install_dir/.mhc.new.$$" "$install_dir/mhc"
fi

# 4. Atomically publish current.json.
if [ -n "$previous" ]; then previous_json="\"$previous\""; else previous_json=null; fi
printf '{"schemaVersion":1,"current":"%s","previous":%s}\n' "$new" "$previous_json" > "$install_dir/.current.json.$$"
mv -f "$install_dir/.current.json.$$" "$install_dir/current.json"

ln -sfn "$install_dir/mhc" "$bin_dir/mhc"

installed=$(payload_version "$install_dir/mhc")
if [ "$installed" != "$new" ]; then
  if [ "$had_pointer" = 1 ]; then mv -f "$work/current.json.bak" "$install_dir/current.json"; fi
  die "installed launcher reports '$installed', expected '$new'$(probe_report)"
fi

say "Installed mhc $installed to $install_dir"
case ":$PATH:" in
  *":$bin_dir:"*) say "Run: mhc" ;;
  *) say "Add $bin_dir to your PATH, e.g.: echo 'export PATH=\"$bin_dir:\$PATH\"' >> ~/.profile" ;;
esac
