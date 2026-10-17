#!/usr/bin/env bash
# Verify the current checkout on GitHub Actions (.github/workflows/ci.yml).
# Usage: scripts/gh-verify.sh [--keep] [--no-wait]
#   Snapshots HEAD plus staged, unstaged and untracked changes into one commit on a temporary
#   `verify/<user>-<timestamp>` branch, pushes it, waits for the ci run and exits with its result.
#   The local branch, index and working tree are never touched. The remote branch is deleted
#   afterwards unless --keep is given.
set -euo pipefail

keep=false
wait=true
for arg in "$@"; do
  case "$arg" in
    --keep) keep=true ;;
    --no-wait) wait=false; keep=true ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

command -v gh >/dev/null || { echo "gh CLI missing" >&2; exit 2; }
if [[ $(git rev-parse --is-bare-repository 2>/dev/null) == true ]]; then
  export GIT_WORK_TREE=$PWD
fi
repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

remote_url=$(git remote get-url origin)
slug=$(printf '%s' "$remote_url" | sed -E 's#(git@github.com:|https://github.com/)##; s#\.git$##')
owner=${slug%%/*}
# Push and query as the repository owner's gh account, never the globally active one.
export GH_TOKEN=${GH_TOKEN:-$(gh auth token --hostname github.com --user "$owner")}

index=$(mktemp)
trap 'rm -f "$index"' EXIT
GIT_INDEX_FILE=$index git read-tree HEAD
GIT_INDEX_FILE=$index git add -A
tree=$(GIT_INDEX_FILE=$index git write-tree)
head=$(git rev-parse HEAD)
if [[ $tree == $(git rev-parse "HEAD^{tree}") ]]; then
  commit=$head
else
  commit=$(printf 'verify: snapshot of %s with local changes\n' "$(git rev-parse --short HEAD)" \
    | git -c user.name=gh-verify -c user.email=gh-verify@example.invalid commit-tree "$tree" -p "$head")
fi

branch="verify/$(whoami)-$(date +%Y%m%d-%H%M%S)"
echo "== push $commit -> $branch"
git push -q "https://x-access-token:${GH_TOKEN}@github.com/${slug}.git" "$commit:refs/heads/$branch"

delete_branch() {
  if [[ $keep == false ]]; then
    git push -q "https://x-access-token:${GH_TOKEN}@github.com/${slug}.git" --delete "$branch" || true
  fi
}

run_id=""
for _ in $(seq 1 30); do
  run_id=$(gh run list -R "$slug" --branch "$branch" --event push --json databaseId,headSha,workflowName \
    -q "map(select(.headSha == \"$commit\" and .workflowName == \"ci\")) | .[0].databaseId // empty")
  [[ -n $run_id ]] && break
  sleep 4
done
if [[ -z $run_id ]]; then
  echo "no ci run appeared for $branch" >&2
  delete_branch
  exit 1
fi
url="https://github.com/${slug}/actions/runs/${run_id}"
echo "== run $url"
if [[ $wait == false ]]; then
  exit 0
fi

status=0
gh run watch -R "$slug" "$run_id" --exit-status --interval 30 >/dev/null || status=$?
gh run view -R "$slug" "$run_id" --json jobs \
  -q '.jobs[] | "\(.conclusion // .status)\t\(.name)\t\((.completedAt | fromdate) - (.startedAt | fromdate))s"'
if [[ $status -ne 0 ]]; then
  echo "== failed steps"
  gh run view -R "$slug" "$run_id" --log-failed 2>/dev/null | tail -n 80 || true
fi
echo "== $( [[ $status -eq 0 ]] && echo PASS || echo FAIL ) $url"
delete_branch
exit "$status"
