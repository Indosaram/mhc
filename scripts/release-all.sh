#!/usr/bin/env bash
# Usage: scripts/release-all.sh <version> [--build-only]
#
# Builds the three release targets on their own hosts, then signs and publishes.
# Host-specific failure modes live in docs/release.md, not here.
set -euo pipefail

version="${1:?usage: release-all.sh <version> [--build-only]}"
mode="${2:-}"
version="${version#v}"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo"

win_host="${MHC_WIN_HOST:-win-new}"
win_protoc="${MHC_WIN_PROTOC:-C:/Users/sook/build/protoc/bin/protoc.exe}"
# /tmp is a RAM-backed tmpfs on Linux hosts; a full tree copy plus bun install
# there consumes memory and dies as "Disk quota exceeded". Stage off the tmpfs.
export TMPDIR="${MHC_RELEASE_TMPDIR:-${TMPDIR:-/var/tmp}}"
mkdir -p "$TMPDIR"
mac_host="${MHC_MAC_HOST:-maho-mac}"
out_root="${MHC_RELEASE_OUT:-$HOME/.cache/mhc-release-$version}"
unix_dir="$out_root/unix"
win_dir="$out_root/windows"
mkdir -p "$unix_dir" "$win_dir"

die() { echo "release-all: $*" >&2; exit 1; }
step() { echo; echo "=== $* ($(date +%H:%M:%S))"; }

step "preflight"
[ "$(git rev-parse --abbrev-ref HEAD)" = "main" ] || die "release from main"
git fetch -q origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || die "HEAD is not pushed to origin/main"
release_commit=$(git rev-parse HEAD)
version_commit=$(git rev-parse --short "$release_commit")
echo "    v$version from $version_commit"

omowright_commit=$(bun -e 'console.log((await Bun.file("assets/omo-latest/manifest.json").json()).browserRuntime.dependency.commit)' | tr -d '\r\n ')
printf '%s' "$omowright_commit" | grep -Eq '^[0-9a-f]{40}$' \
  || die "could not read the omowright commit from assets/omo-latest/manifest.json"
echo "    omowright $omowright_commit"

step "linux-x86_64 (local)"
[ "$(git rev-parse HEAD)" = "$release_commit" ] || die "local checkout moved during the release"
bash scripts/release-linux.sh "$version" "$unix_dir"

step "windows-x86_64 ($win_host)"
win_root="C:/Users/sook/build/rel-$version"
win_bs="C:\\Users\\sook\\build\\rel-$version"
ssh -o BatchMode=yes "$win_host" "if exist $win_bs\\src\\.git (cd /d $win_bs\\src && git fetch -q --depth 1 origin $release_commit && git checkout -q --force FETCH_HEAD) else (git init -q $win_bs\\src && cd /d $win_bs\\src && git remote add origin https://github.com/Indosaram/mhc.git && git fetch -q --depth 1 origin $release_commit && git checkout -q --force FETCH_HEAD)" \
  || die "could not prepare $win_root/src on $win_host"
ssh -o BatchMode=yes "$win_host" "cd /d $win_bs\\src && git rev-parse HEAD" | tr -d '\r' | grep -q "^$release_commit$" \
  || die "$win_host is not on $version_commit"
ssh -o BatchMode=yes "$win_host" "pwsh -NoProfile -ExecutionPolicy Bypass -File $win_bs\\src\\scripts\\release-windows.ps1 -Version $version -OutputDir $win_bs\\out -OmowrightCommit $omowright_commit -ProtocPath $win_protoc" \
  || die "windows build failed"
scp -o BatchMode=yes "$win_host:$win_root/out/*" "$win_dir/" || die "could not collect windows assets"

step "macos-aarch64 ($mac_host)"
if ssh -o BatchMode=yes -o ConnectTimeout=8 "$mac_host" true 2>/dev/null; then
  mac_home=$(ssh -o BatchMode=yes "$mac_host" 'printf %s "$HOME"' | tr -d '\r') \
    || die "could not resolve home on $mac_host"
  mac_root="$mac_home/build/rel-$version"
  ssh -o BatchMode=yes "$mac_host" "if [ -d $mac_root/src/.git ]; then git -C $mac_root/src fetch -q --depth 1 origin $release_commit && git -C $mac_root/src checkout -q --force FETCH_HEAD; else mkdir -p $mac_root && git init -q $mac_root/src && git -C $mac_root/src remote add origin https://github.com/Indosaram/mhc.git && git -C $mac_root/src fetch -q --depth 1 origin $release_commit && git -C $mac_root/src checkout -q --force FETCH_HEAD; fi" \
    || die "could not prepare macos checkout"
  ssh -o BatchMode=yes "$mac_host" "cd $mac_root/src && git rev-parse HEAD" | tr -d '\r' | grep -q "^$release_commit$" \
    || die "$mac_host is not on $version_commit"
  ssh -o BatchMode=yes "$mac_host" "cd $mac_root/src && bash scripts/release-macos.sh $version $mac_root/out" \
    || die "macos build failed"
  scp -o BatchMode=yes "$mac_host:$mac_root/out/*" "$unix_dir/" || die "could not collect macos assets"
else
  die "$mac_host unreachable over ssh; see docs/release.md for the Ferryx relay path"
fi

step "checksums"
for f in "$unix_dir"/*.tar.gz "$win_dir"/*.zip; do
  [ -e "$f" ] || continue
  (cd "$(dirname "$f")" && sha256sum "$(basename "$f")" > "$(basename "$f").sha256")
done

if [ "$mode" = "--build-only" ]; then
  echo
  echo "build-only: assets in $out_root"
  find "$out_root" \( -name '*.tar.gz' -o -name '*.zip' \) -printf '%f  %s bytes\n' | sort
  exit 0
fi

step "sign"
key="${MHC_RELEASE_KEY:-$HOME/.config/mhc-release/mhc-release.key}"
[ -f "$key" ] || die "release key not found: $key"
for f in "$unix_dir"/*.tar.gz "$win_dir"/*.zip; do
  [ -e "$f" ] || continue
  minisign -S -s "$key" -m "$f" -x "$f.minisig" || die "signing failed: $f"
done
cp "${MHC_RELEASE_PUBKEY:-$HOME/.config/mhc-release/mhc-release.pub}" "$out_root/mhc-release.pub" 2>/dev/null || true

step "publish"
gh release create "v$version" --repo Indosaram/mhc --title "mhc v$version" --draft \
  "$unix_dir"/*.tar.gz "$unix_dir"/*.sha256 "$unix_dir"/*.minisig \
  "$win_dir"/*.zip "$win_dir"/*.sha256 "$win_dir"/*.minisig \
  "$out_root/mhc-release.pub" \
  || die "draft release creation failed"
gh release edit "v$version" --repo Indosaram/mhc --draft=false || die "publish failed"
gh release view "v$version" --repo Indosaram/mhc --json tagName,assets -q '.tagName, (.assets[].name)'

step "cleanup"
rm -rf "$out_root"
ssh -o BatchMode=yes "$win_host" "if exist $win_bs rmdir /s /q $win_bs" 2>/dev/null || true
echo "done"
