#!/usr/bin/env bash
# Usage: scripts/release-all.sh <version> [--build-only]
#
# Builds the three release targets on their own hosts, then signs and publishes.
# Host-specific failure modes live in docs/release.md, not here.
#
# Nothing host-specific is tracked: the ssh destinations, the remote base directories and
# the tool paths come from the environment, or from a file named by MHC_RELEASE_ENV. See
# "Configuration" in docs/release.md.
#
# Every target is built from one commit: this checkout supplies that commit and nothing
# else. Linux builds in a clean clone of it, the remote hosts fetch it and clean their
# checkout, and the omowright pin is read out of the commit instead of the working tree.
set -euo pipefail

version="${1:?usage: release-all.sh <version> [--build-only]}"
mode="${2:-}"
version="${version#v}"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo"

die() { echo "release-all: $*" >&2; exit 1; }
step() { echo; echo "=== $* ($(date +%H:%M:%S))"; }

# Quote for a POSIX shell (the mac host) and for PowerShell 7 (the Windows host, whose ssh
# login shell it is; docs/release.md "Windows: the SSH shell is already PowerShell").
# %q would be simpler but only bash's own sh mode reads it back the same way.
sh_quote() {
  local value=$1
  printf "'%s'" "${value//\'/\'\\\'\'}"
}
ps_quote() {
  local value=$1
  printf "'%s'" "${value//\'/\'\'}"
}

case "$mode" in
  "" | --build-only) ;;
  *) die "unknown mode '$mode' (expected --build-only)" ;;
esac
# The version reaches remote shells and PowerShell string literals; keep it to characters
# that need no quoting anywhere.
printf '%s' "$version" | grep -Eq '^[0-9A-Za-z]+([._-][0-9A-Za-z]+)*$' \
  || die "unexpected version '$version'"

release_env="${MHC_RELEASE_ENV:-}"
if [ -n "$release_env" ]; then
  [ -f "$release_env" ] || die "MHC_RELEASE_ENV=$release_env does not exist"
  # shellcheck disable=SC1090,SC1091
  . "$release_env"
fi

win_host="${MHC_WIN_HOST:-}"
mac_host="${MHC_MAC_HOST:-}"
win_protoc="${MHC_WIN_PROTOC:-}"
win_base=""
mac_base=""
# The per-target scripts read this from the environment, so a bare assignment inside the
# sourced file would otherwise never reach them.
if [ "${MHC_RELEASE_SKIP_MIT:-}" = 1 ]; then
  export MHC_RELEASE_SKIP_MIT=1
fi
[ -n "$win_host" ] || die "set MHC_WIN_HOST (environment, or a file named by MHC_RELEASE_ENV): host names stay out of this repository"
[ -n "$mac_host" ] || die "set MHC_MAC_HOST (environment, or a file named by MHC_RELEASE_ENV): host names stay out of this repository"

# /tmp is a RAM-backed tmpfs on Linux hosts; a full tree copy plus bun install
# there consumes memory and dies as "Disk quota exceeded". Stage off the tmpfs.
export TMPDIR="${MHC_RELEASE_TMPDIR:-${TMPDIR:-/var/tmp}}"
mkdir -p "$TMPDIR"

linux_cache="${MHC_RELEASE_CACHE:-$HOME/.cache/mhc-release-build}"

release_target="${MHC_RELEASE_TARGET_DIR:-$linux_cache/target}"
case "$release_target" in
  /*) ;;
  *) release_target="$repo/$release_target" ;;
esac
export CARGO_TARGET_DIR="$release_target"

# MHC_RELEASE_OUT names the *parent* of this run's directory. Only the directory this
# script creates is ever deleted, so a pre-existing output directory keeps everything
# that was in it before the release.
out_base="${MHC_RELEASE_OUT:-$HOME/.cache/mhc-release}"
mkdir -p "$out_base"
out_base="$(cd "$out_base" && pwd)"
out_run="$(mktemp -d "$out_base/$version-XXXXXXXX")"
run_name="rel-${out_run##*/}"
linux_src="$out_run/linux-src"
unix_dir="$out_run/unix"
win_dir="$out_run/windows"
mkdir -p "$unix_dir" "$win_dir"

# The commit tag "$tag" points at on origin, or empty when the tag does not exist yet.
# `git ls-remote` is used instead of `gh` so a missing login cannot be mistaken for a
# missing tag; a failed lookup is fatal. Annotated tags list a peeled `^{}` ref, which is
# the commit, so prefer it over the tag object's own sha.
remote_tag_commit() {
  local refs peeled
  refs=$(git ls-remote --tags origin "refs/tags/$tag" "refs/tags/$tag^{}") || return 1
  [ -n "$refs" ] || return 0
  peeled=$(printf '%s\n' "$refs" | awk -v want="refs/tags/$tag^{}" '$2 == want { print $1 }')
  if [ -n "$peeled" ]; then
    printf '%s' "$peeled"
  else
    printf '%s\n' "$refs" | awk -v want="refs/tags/$tag" '$2 == want { print $1 }'
  fi
}

step "preflight"
[ "$(git rev-parse --abbrev-ref HEAD)" = "main" ] || die "release from main"
git fetch -q origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || die "HEAD is not pushed to origin/main"
release_commit=$(git rev-parse HEAD)
version_commit=$(git rev-parse --short "$release_commit")
tag="v$version"
echo "    v$version from $version_commit"
if [ -n "$(git status --porcelain)" ]; then
  echo "    note: this checkout has uncommitted work; every target is built from $version_commit, not from it"
fi

if [ "$mode" = "--build-only" ]; then
  echo "    build-only: no tag is queried, created or checked"
else
  existing_tag=$(remote_tag_commit) || die "could not read refs/tags/$tag from origin"
  if [ -n "$existing_tag" ]; then
    [ "$existing_tag" = "$release_commit" ] \
      || die "tag $tag already points at ${existing_tag:0:12}, not $version_commit; use the next version"
    echo "    tag $tag already at $version_commit"
  else
    echo "    tag $tag will be created at $release_commit"
  fi
fi

omowright_commit=$(git show "$release_commit:assets/omo-latest/manifest.json" \
  | node -e 'let s="";process.stdin.setEncoding("utf8");process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(String(JSON.parse(s).browserRuntime.dependency.commit||"")))') \
  || die "could not read assets/omo-latest/manifest.json at $version_commit"
printf '%s' "$omowright_commit" | grep -Eq '^[0-9a-f]{40}$' \
  || die "assets/omo-latest/manifest.json at $version_commit has no browserRuntime.dependency.commit"
echo "    omowright $omowright_commit"

step "linux-x86_64 (local)"
git init -q "$linux_src"
git -C "$linux_src" fetch -q --depth 1 "$repo" "$release_commit"
git -C "$linux_src" checkout -q --detach FETCH_HEAD
[ "$(git -C "$linux_src" rev-parse HEAD)" = "$release_commit" ] \
  || die "the clean checkout is not on $version_commit"
bash "$linux_src/scripts/release-linux.sh" "$version" "$unix_dir"

step "windows-x86_64 ($win_host)"
win_base="${MHC_WIN_BASE:-}"
if [ -z "$win_base" ]; then
  win_base=$(ssh -o BatchMode=yes "$win_host" '$env:USERPROFILE' | tr -d '\r') \
    || die "could not resolve USERPROFILE on $win_host"
  win_base="$win_base/build"
fi
win_base=$(printf '%s' "$win_base" | tr '\\' '/' | sed 's#/*$##')
win_src="$win_base/$run_name/src"
win_out="$win_base/$run_name/out"

# The remote login shell is PowerShell 7, so the checkout, the SHA check and the cleanup
# are PowerShell too. cmd.exe's `if exist`, `cd /d` and `rmdir /s /q` die before the build
# script is reached. Continue plus explicit $LASTEXITCODE checks: PowerShell treats a
# native command's stderr output as fatal under Stop (release-windows.ps1 says the same).
# `clean -qdf` keeps ignored files (build output worth staying warm) and removes untracked
# ones, which are exactly the stale source a reused checkout could otherwise ship.
win_prepare=$(printf '%s\n' \
  "\$ErrorActionPreference = 'Continue'" \
  "\$src = $(ps_quote "$win_src")" \
  "if (-not (Test-Path (Join-Path \$src '.git'))) {" \
  "  New-Item -ItemType Directory -Force -Path \$src | Out-Null" \
  "  git init -q \$src" \
  "  if (\$LASTEXITCODE -ne 0) { exit 1 }" \
  "  git -C \$src remote add origin https://github.com/Indosaram/mhc.git" \
  "  if (\$LASTEXITCODE -ne 0) { exit 1 }" \
  "}" \
  "git -C \$src fetch -q --depth 1 origin $release_commit" \
  "if (\$LASTEXITCODE -ne 0) { exit 1 }" \
  "git -C \$src checkout -q --force FETCH_HEAD" \
  "if (\$LASTEXITCODE -ne 0) { exit 1 }" \
  "git -C \$src clean -qdf" \
  "if (\$LASTEXITCODE -ne 0) { exit 1 }" \
  "exit 0")
ssh -o BatchMode=yes "$win_host" "$win_prepare" || die "could not prepare $win_src on $win_host"
ssh -o BatchMode=yes "$win_host" "git -C $(ps_quote "$win_src") rev-parse HEAD" | tr -d '\r' | grep -qx "$release_commit" \
  || die "$win_host is not on $version_commit"

win_build="pwsh -NoProfile -ExecutionPolicy Bypass -File $(ps_quote "$win_src/scripts/release-windows.ps1") -Version $(ps_quote "$version") -OutputDir $(ps_quote "$win_out") -OmowrightCommit $(ps_quote "$omowright_commit")"
if [ -n "$win_protoc" ]; then
  win_build="$win_build -ProtocPath $(ps_quote "$win_protoc")"
fi
if [ "${MHC_RELEASE_SKIP_MIT:-}" = 1 ]; then
  win_build="$win_build -SkipMit"
fi
ssh -o BatchMode=yes "$win_host" "$win_build" || die "windows build failed"
# scp hands the remote path to that host as given, so it cannot carry quoting or spaces; the
# base directories are documented as space-free for this reason.
scp -o BatchMode=yes "$win_host:$win_out/*" "$win_dir/" || die "could not collect windows assets"

step "macos-aarch64 ($mac_host)"
if ssh -o BatchMode=yes -o ConnectTimeout=8 "$mac_host" true 2>/dev/null; then
  mac_base="${MHC_MAC_BASE:-}"
  if [ -z "$mac_base" ]; then
    mac_home=$(ssh -o BatchMode=yes "$mac_host" 'printf %s "$HOME"' | tr -d '\r') \
      || die "could not resolve home on $mac_host"
    mac_base="$mac_home/build"
  fi
  mac_base=$(printf '%s' "$mac_base" | sed 's#/*$##')
  mac_src="$mac_base/$run_name/src"
  mac_out="$mac_base/$run_name/out"
  mac_prepare=$(printf '%s\n' \
    "src=$(sh_quote "$mac_src")" \
    "if [ -d \"\$src/.git\" ]; then" \
    "  git -C \"\$src\" fetch -q --depth 1 origin $release_commit || exit 1" \
    "  git -C \"\$src\" checkout -q --force FETCH_HEAD || exit 1" \
    "else" \
    "  mkdir -p \"\$src\" || exit 1" \
    "  git init -q \"\$src\" || exit 1" \
    "  git -C \"\$src\" remote add origin https://github.com/Indosaram/mhc.git || exit 1" \
    "  git -C \"\$src\" fetch -q --depth 1 origin $release_commit || exit 1" \
    "  git -C \"\$src\" checkout -q --force FETCH_HEAD || exit 1" \
    "fi" \
    "git -C \"\$src\" clean -qdf")
  ssh -o BatchMode=yes "$mac_host" "$mac_prepare" || die "could not prepare $mac_src on $mac_host"
  ssh -o BatchMode=yes "$mac_host" "git -C $(sh_quote "$mac_src") rev-parse HEAD" | tr -d '\r' | grep -qx "$release_commit" \
    || die "$mac_host is not on $version_commit"
  mac_build="cd $(sh_quote "$mac_src") &&"
  if [ "${MHC_RELEASE_SKIP_MIT:-}" = 1 ]; then
    mac_build="$mac_build MHC_RELEASE_SKIP_MIT=1"
  fi
  mac_build="$mac_build bash scripts/release-macos.sh $(sh_quote "$version") $(sh_quote "$mac_out")"
  ssh -o BatchMode=yes "$mac_host" "$mac_build" || die "macos build failed"
  scp -o BatchMode=yes "$mac_host:$mac_out/*" "$unix_dir/" || die "could not collect macos assets"
else
  die "$mac_host is unreachable over ssh; host details and routing live in the private build-host note AGENTS.md names (docs/release.md)"
fi

step "checksums"
for f in "$unix_dir"/*.tar.gz "$win_dir"/*.zip; do
  [ -e "$f" ] || continue
  (cd "$(dirname "$f")" && sha256sum "$(basename "$f")" > "$(basename "$f").sha256")
done

if [ "$mode" = "--build-only" ]; then
  echo
  echo "build-only: assets in $out_run; the Linux checkout is at $linux_src"
  find "$out_run" \( -name '*.tar.gz' -o -name '*.zip' \) -printf '%f  %s bytes\n' | sort
  exit 0
fi

step "sign"
key="${MHC_RELEASE_KEY:-$HOME/.config/mhc-release/mhc-release.key}"
[ -f "$key" ] || die "release key not found: $key"
for f in "$unix_dir"/*.tar.gz "$win_dir"/*.zip; do
  [ -e "$f" ] || continue
  minisign -S -s "$key" -m "$f" -x "$f.minisig" || die "signing failed: $f"
done
cp "${MHC_RELEASE_PUBKEY:-$HOME/.config/mhc-release/mhc-release.pub}" "$out_run/mhc-release.pub" 2>/dev/null || true

step "publish"
# Re-read the tag now: another run could have created it during the build, and --target
# would then fail late or, worse, a moved tag would ship assets for other sources.
published_before=$(remote_tag_commit) || die "could not read refs/tags/$tag from origin"
if [ -n "$published_before" ]; then
  [ "$published_before" = "$release_commit" ] \
    || die "tag $tag points at ${published_before:0:12}, not $version_commit"
  tag_args=(--verify-tag)
else
  tag_args=(--target "$release_commit")
fi
gh release create "$tag" "${tag_args[@]}" --repo Indosaram/mhc --title "mhc $tag" --draft \
  "$unix_dir"/*.tar.gz "$unix_dir"/*.sha256 "$unix_dir"/*.minisig \
  "$win_dir"/*.zip "$win_dir"/*.sha256 "$win_dir"/*.minisig \
  "$out_run/mhc-release.pub" \
  || die "draft release creation failed"
gh release edit "$tag" --repo Indosaram/mhc --draft=false || die "publish failed"
published_after=$(remote_tag_commit) || die "could not read refs/tags/$tag from origin after publishing"
[ "$published_after" = "$release_commit" ] \
  || die "tag $tag does not point at $version_commit after publishing"
gh release view "$tag" --repo Indosaram/mhc --json tagName,assets -q '.tagName, (.assets[].name)'

step "cleanup"
case "$out_run" in
  "$out_base"/*) rm -rf "$out_run" ;;
  *) die "refusing to remove $out_run: it is not inside $out_base" ;;
esac
if [ -n "$win_base" ]; then
  win_cleanup=$(printf '%s\n' \
    "\$target = $(ps_quote "$win_base/$run_name")" \
    "if (Test-Path \$target) { Remove-Item -Recurse -Force \$target }" \
    "exit 0")
  ssh -o BatchMode=yes "$win_host" "$win_cleanup" 2>/dev/null || true
fi
if [ -n "$mac_base" ]; then
  ssh -o BatchMode=yes "$mac_host" "rm -rf $(sh_quote "$mac_base/$run_name")" 2>/dev/null || true
fi
echo "done"
