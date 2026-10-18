#!/usr/bin/env bash
# Build a release mhc on GitHub Actions (.github/workflows/mhc-binary.yml) and install it locally
# as a dated dev build (YYYY.MM.DD-N, N counts installs made on that date).
# Snapshots HEAD plus staged, unstaged and untracked changes into one commit on a temporary
# `build/<version>` branch; the local branch, index and working tree are never touched.
set -euo pipefail

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

command -v gh >/dev/null || { echo "gh CLI missing" >&2; exit 2; }
if [[ $(git rev-parse --is-bare-repository 2>/dev/null) == true ]]; then
  export GIT_WORK_TREE=$PWD
fi
cd "$(git rev-parse --show-toplevel)"
slug=$(git remote get-url origin | sed -E 's#(git@github.com:|https://github.com/)##; s#\.git$##')
# Push and query as the repository owner's gh account, never the globally active one.
export GH_TOKEN=${GH_TOKEN:-$(gh auth token --hostname github.com --user "${slug%%/*}")}
push_url="https://x-access-token:${GH_TOKEN}@github.com/${slug}.git"

mkdir -p "$install_dir" "$bin_dir"
# The ~200 MB binary does not fit in a quota-limited /tmp, so stage it beside the install.
work=$(mktemp -d "$install_dir/.gh-install.XXXXXX")
branch="build/$version"
cleanup() { rm -rf "$work"; git push -q "$push_url" --delete "$branch" 2>/dev/null || true; }
trap cleanup EXIT

GIT_INDEX_FILE=$work/index git read-tree HEAD
GIT_INDEX_FILE=$work/index git add -A
tree=$(GIT_INDEX_FILE=$work/index git write-tree)
commit=$(git rev-parse HEAD)
if [[ $tree != $(git rev-parse "HEAD^{tree}") ]]; then
  commit=$(printf 'build: snapshot of %s with local changes\n' "$(git rev-parse --short HEAD)" \
    | git -c user.name=gh-install -c user.email=gh-install@example.invalid commit-tree "$tree" -p HEAD)
fi
echo "== push $commit -> $branch"
git push -q "$push_url" "$commit:refs/heads/$branch"

run_id=""
for _ in $(seq 1 30); do
  run_id=$(gh run list -R "$slug" --branch "$branch" --event push --json databaseId,headSha,workflowName \
    -q "map(select(.headSha == \"$commit\" and .workflowName == \"mhc-binary\")) | .[0].databaseId // empty")
  [[ -n $run_id ]] && break
  sleep 4
done
[[ -n $run_id ]] || { echo "no mhc-binary run appeared for $branch" >&2; exit 1; }
echo "== run https://github.com/${slug}/actions/runs/${run_id}"
if ! gh run watch -R "$slug" "$run_id" --exit-status --interval 30 >/dev/null; then
  gh run view -R "$slug" "$run_id" --log-failed 2>/dev/null | tail -n 80 || true
  exit 1
fi

gh run download -R "$slug" "$run_id" -n mhc-linux-x86_64 -D "$work/out"
install -m 755 "$work/out/mhc" "$install_dir/mhc.new"
mv "$install_dir/mhc.new" "$install_dir/mhc"
ln -sfn "$install_dir/mhc" "$bin_dir/mhc"

installed="$("$bin_dir/mhc" --version)"
if [ "$installed" != "$version" ]; then
  echo "installed mhc reports '$installed', expected '$version'" >&2
  exit 1
fi
echo "installed mhc $installed"
