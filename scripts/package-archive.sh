#!/usr/bin/env bash
# Archive a verified package directory as <name>.tar.gz plus <name>.tar.gz.sha256 and
# check the archive the way `mhc update` will read it.
#
# Usage: scripts/package-archive.sh <dist-dir> <name>
#   <dist-dir>/<name>/  package directory (already checked by package-native.mjs --verify-only)
#   writes <dist-dir>/<name>.tar.gz and <dist-dir>/<name>.tar.gz.sha256
#
# macOS bsdtar otherwise adds AppleDouble `._*` entries and xattr pax headers
# (com.apple.provenance); a top-level `._<name>` makes `mhc update` fail with
# "archive must contain exactly one package directory".
set -euo pipefail

dist="${1:?usage: package-archive.sh <dist-dir> <name>}"
name="${2:?usage: package-archive.sh <dist-dir> <name>}"
archive="$dist/$name.tar.gz"
test -d "$dist/$name"

flags=()
if tar --version 2>/dev/null | grep -q bsdtar; then
  flags=(--no-mac-metadata --no-xattrs)
fi
rm -f "$archive" "$archive.sha256"
COPYFILE_DISABLE=1 tar "${flags[@]}" -C "$dist" -czf "$archive" "$name"

listing=$(tar -tzf "$archive" | sed -e 's#^\./##')
bad=$(printf '%s\n' "$listing" | awk -v top="$name" -F/ '
  $0 == "" { next }
  $1 != top { print "outside " top "/: " $0; next }
  { for (i = 1; i <= NF; i++) if ($i ~ /^\._/ || $i == "__MACOSX" || $i == ".DS_Store") { print "macOS metadata: " $0; next } }
')
if [ -n "$bad" ]; then
  printf '%s is not a single clean package directory:\n%s\n' "$archive" "$(printf '%s\n' "$bad" | head -20)" >&2
  exit 1
fi
for exe in mhc mhc-launcher; do
  printf '%s\n' "$listing" | grep -Fx "$name/$exe" > /dev/null || { echo "$archive lacks $name/$exe" >&2; exit 1; }
done

if command -v sha256sum > /dev/null; then
  (cd "$dist" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
else
  (cd "$dist" && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")
fi
echo "wrote $archive"
