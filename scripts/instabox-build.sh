#!/usr/bin/env bash
# Run a cargo command for the current checkout on an oldbox42 sandbox.
# Usage: scripts/oldbox42-build.sh [-m MACHINE] [-k g|h] [-- cargo args...]
#   default machine bong-g-1, default key g, default cargo args: build -p maho-cli
# Keys come from ~/.config/oldbox42/keys.env (INSTABOX_KEY_G / INSTABOX_KEY_H); never commit them.
set -euo pipefail

machine=bong-g-1
key=g
while [[ $# -gt 0 ]]; do
  case "$1" in
    -m) machine=$2; shift 2 ;;
    -k) key=$2; shift 2 ;;
    --) shift; break ;;
    *) break ;;
  esac
done
cargo_args=("$@")
[[ ${#cargo_args[@]} -gt 0 ]] || cargo_args=(build -p maho-cli)

keys_file="$HOME/.config/oldbox42/keys.env"
[[ -r $keys_file ]] || { echo "missing $keys_file" >&2; exit 2; }
# shellcheck disable=SC1090
source "$keys_file"
key_var="INSTABOX_KEY_$(printf '%s' "$key" | tr '[:lower:]' '[:upper:]')"
export INSTABOX_API_KEY="${!key_var:?no key $key_var in $keys_file}"
command -v oldbox42 >/dev/null || { echo "oldbox42 CLI missing: npm i -g oldbox42@latest" >&2; exit 2; }

if [[ $(git rev-parse --is-bare-repository 2>/dev/null) == true ]]; then
  export GIT_WORK_TREE=$PWD
fi
repo=$(git rev-parse --show-toplevel)
cd "$repo"
git fetch -q origin main
base=$(git merge-base HEAD origin/main)

box() { oldbox42 exec "$machine" --timeout 600 --stdin -- bash; }

echo "== bootstrap $machine (rustup 1.95.0, protoc, clone at $base)"
box <<EOF
set -e
export RUSTUP_HOME=\$HOME/.rustup CARGO_HOME=\$HOME/.cargo
if [ ! -x \$CARGO_HOME/bin/rustup ]; then
  curl -sSf https://sh.rustup.rs | sh -s -- -y -q --no-modify-path --default-toolchain none
fi
\$CARGO_HOME/bin/rustup toolchain list | grep -q '^1.95.0' || \$CARGO_HOME/bin/rustup toolchain install 1.95.0 --profile minimal -c clippy,rustfmt,rust-src
if [ ! -x \$HOME/protoc/bin/protoc ]; then
  curl -sSLo \$HOME/protoc.zip https://github.com/protocolbuffers/protobuf/releases/download/v29.3/protoc-29.3-linux-x86_64.zip
  mkdir -p \$HOME/protoc && python3 -c "import zipfile;zipfile.ZipFile('\$HOME/protoc.zip').extractall('\$HOME/protoc')" && chmod +x \$HOME/protoc/bin/protoc
fi
[ -d \$HOME/mhc/.git ] || git clone -q https://github.com/Indosaram/mhc.git \$HOME/mhc
mkdir -p \$CARGO_HOME
cat > \$CARGO_HOME/config.toml <<'TOML'
[profile.dev]
debug = "line-tables-only"
incremental = false

[profile.dev.package."*"]
debug = false

[profile.test]
debug = "line-tables-only"
incremental = false
TOML
if [ "\$(df --output=avail -k /data | tail -1)" -lt 6000000 ]; then rm -rf \$HOME/mhc/target; fi
cd \$HOME/mhc
git fetch -q origin
git reset -q --hard $base
git clean -qfd
EOF

echo "== sync local changes (commits after origin/main + uncommitted + untracked)"
tmp_index=$(mktemp)
cp "$(git rev-parse --git-path index)" "$tmp_index"
GIT_INDEX_FILE=$tmp_index git add -A
while IFS= read -r -d '' f; do
  if [[ -f $f && $(wc -c < "$f") -gt 1000000 ]]; then
    echo "   skip large untracked file: $f"
    GIT_INDEX_FILE=$tmp_index git rm -q --cached -- "$f"
  fi
done < <(git ls-files -z --others --exclude-standard)
GIT_INDEX_FILE=$tmp_index git diff --cached --binary "$base" > "$tmp_index.patch"
rm -f "$tmp_index"
if [[ -s $tmp_index.patch ]]; then
  echo "   diff $(wc -c < "$tmp_index.patch" | tr -d ' ') bytes"
  oldbox42 exec "$machine" -- rm -f /data/home/sync.patch
  split -b 900000 "$tmp_index.patch" "$tmp_index.part."
  for part in "$tmp_index".part.*; do
    oldbox42 exec "$machine" --stdin -- 'cat >> /data/home/sync.patch' < "$part"
  done
  rm -f "$tmp_index".part.*
  oldbox42 exec "$machine" -- git -C /data/home/mhc apply --whitespace=nowarn /data/home/sync.patch
fi
rm -f "$tmp_index.patch"

printf -v cmd '%q ' cargo "${cargo_args[@]}"
echo "== run on $machine: $cmd"
box <<EOF
cd \$HOME/mhc
rm -f \$HOME/oldbox42-run.log
setsid nohup bash -c 'export RUSTUP_HOME=\$HOME/.rustup CARGO_HOME=\$HOME/.cargo PATH=\$HOME/.cargo/bin:\$PATH PROTOC=\$HOME/protoc/bin/protoc PROTOC_INCLUDE=\$HOME/protoc/include; s=\$(date +%s); $cmd > \$HOME/oldbox42-run.log 2>&1; echo "EXIT=\$? WALL=\$(( \$(date +%s)-s ))s" >> \$HOME/oldbox42-run.log' < /dev/null > /dev/null 2>&1 &
EOF

until oldbox42 exec "$machine" -- grep -q '^EXIT=' /data/home/oldbox42-run.log >/dev/null 2>&1; do sleep 20; done
oldbox42 exec "$machine" -- tail -n 40 /data/home/oldbox42-run.log
oldbox42 exec "$machine" -- grep -q '^EXIT=0 ' /data/home/oldbox42-run.log
