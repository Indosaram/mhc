set -euo pipefail
driver=$(realpath "$1")
cd "$2"
export HOME="$PWD/home" MHC_RELEASE_OUT="$PWD/out" MHC_RELEASE_CACHE="$PWD/cache" MHC_RELEASE_TMPDIR="$PWD/tmp" MHC_WIN_HOST=win.test MHC_MAC_HOST=mac.test MHC_WIN_BASE=C:/build MHC_MAC_BASE=/build
export MHC_RELEASE_KEY="$PWD/key" MHC_RELEASE_PUBKEY="$PWD/pub" MHC_RELEASE_ENV='' MHC_RELEASE_TARGET_DIR="$PWD/target" GIT_CONFIG_NOSYSTEM=1
export FIXTURE_ROOT="$PWD" PATH="$PWD/bin:$PATH"
mkdir -p "$HOME" bin out cache/linux-src repo/scripts repo/assets/omo-latest
touch out/keep cache/linux-src/keep key pub
cp "$driver" repo/scripts/release-all.sh
printf '%s\n' '{"browserRuntime":{"dependency":{"commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}}' > repo/assets/omo-latest/manifest.json
printf '%s\n' 'set -eu' 'test "$(git -C "$(dirname "$0")" rev-parse HEAD)" = "$PIN"' 'test "${MHC_RELEASE_SKIP_MIT:-}" = "$EXPECTED_SKIP"' 'touch "$2/linux.tar.gz"' > repo/scripts/release-linux.sh
git -C repo init -q -b main
git -C repo add .
git -C repo -c user.name=Fixture -c user.email=fixture@example.test -c commit.gpgsign=false commit -qm initial
PIN=$(git -C repo rev-parse HEAD); export PIN
git clone -q --bare repo origin.git
git -C repo remote add origin "$PWD/origin.git"
printf 'exit 70\n' > repo/scripts/release-linux.sh
printf 'broken\n' > repo/assets/omo-latest/manifest.json
cat > bin/ssh <<'SH'
#!/bin/sh
case "$*" in
  *'cmd /c '*|*'rmdir /s '*|*'if exist '*|*'cd /d '*) exit 90 ;;
  *'-File '*'-OutputDir '*) if [ "$EXPECTED_SKIP" = 1 ]; then case "$*" in *'-SkipMit'*) : ;; *) exit 93 ;; esac; fi ;;
  *'bash scripts/release-macos.sh '*) if [ "$EXPECTED_SKIP" = 1 ]; then case "$*" in *'MHC_RELEASE_SKIP_MIT=1 bash '*) : ;; *) exit 94 ;; esac; fi ;;
  *'rev-parse HEAD'*) printf '%s\n' "$PIN" ;;
esac
SH
cat > bin/scp <<'SH'
#!/bin/bash
dest=${!#}
case "$*" in *win.test:*) touch "$dest/windows.zip" ;; *) touch "$dest/mac.tar.gz" ;; esac
SH
cat > bin/minisign <<'SH'
#!/bin/bash
touch "${!#}"
SH
cat > bin/gh <<'SH'
#!/bin/bash
set -eu
if [[ $2 == create ]]; then
    tag=$3; shift 3
    while (($#)); do
        case "$1" in
            --target) test "$2" = "$PIN"; git --git-dir="$FIXTURE_ROOT/origin.git" update-ref "refs/tags/$tag" "$2"; exit ;;
            --verify-tag) test "$(git --git-dir="$FIXTURE_ROOT/origin.git" rev-parse "$tag^{commit}")" = "$PIN"; exit ;;
        esac
        shift
    done
    exit 91
fi
SH
chmod +x bin/*
export EXPECTED_SKIP='' MHC_RELEASE_SKIP_MIT=''
bash repo/scripts/release-all.sh 1
test -f out/keep
test -f cache/linux-src/keep
test "$(git --git-dir=origin.git rev-parse v1)" = "$PIN"
EXPECTED_SKIP=1 MHC_RELEASE_SKIP_MIT=1 MHC_RELEASE_KEY="$PWD/absent" bash repo/scripts/release-all.sh 2 --build-only
runs=(out/2-*)
test -f "${runs[0]}/unix/linux.tar.gz"
test -f "${runs[0]}/unix/mac.tar.gz"
test -f "${runs[0]}/windows/windows.zip"
if git --git-dir=origin.git show-ref --verify --quiet refs/tags/v2; then exit 95; fi
git -C repo -c user.name=Fixture -c user.email=fixture@example.test -c commit.gpgsign=false commit -qm next --allow-empty
git -C repo push -q origin main
if bash repo/scripts/release-all.sh 1; then exit 92; fi
