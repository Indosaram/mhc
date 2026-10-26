#!/usr/bin/env bash
# mhc vs OmO memory benchmark: the same fixed workflow DAG, each run alone in a fresh container.
# Usage: tools/membench/bench.sh            (1 warm-up + 3 measured runs per agent, alternating)
# Env:   RUNS=3          measured runs per agent
#        AGENTS="mhc omo"
#        WORK=~/.cache/mhc-membench   scratch dir (config copies, results in $WORK/out)
#        MHC_DIR=~/.local/share/mhc   installed mhc package dir (contains the mhc binary)
#        BUN_DIR=~/.bun               bun install with omo-ai installed globally
#        NODE_DIR=<dir of `node`>/..  Node install; OmO starts child agents with `env node`
#        OMO_HOME=~/.omo              shared mhc/OmO config home (models, auth, omo.json)
# Docs: docs/memory-benchmark.md
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
RUNS=${RUNS:-3}
AGENTS=${AGENTS:-"mhc omo"}
WORK=${WORK:-$HOME/.cache/mhc-membench}
MHC_DIR=${MHC_DIR:-$HOME/.local/share/mhc}
BUN_DIR=${BUN_DIR:-$HOME/.bun}
NODE_DIR=${NODE_DIR:-$(dirname "$(dirname "$(readlink -f "$(command -v node)")")")}
OMO_HOME=${OMO_HOME:-$HOME/.omo}
IMAGE=mhc-membench:1
UIDGID="$(id -u):$(id -g)"

for path in "$MHC_DIR/mhc" "$BUN_DIR/bin/omo" "$NODE_DIR/bin/node" "$OMO_HOME/agent/models.json"; do
  [ -e "$path" ] || { echo "missing: $path" >&2; exit 2; }
done

docker build -q -t "$IMAGE" "$HERE" >/dev/null
mkdir -p "$WORK/out"; chmod 777 "$WORK/out"; rm -f "$WORK/out"/*

# Template home: only the parts of ~/.omo both agents read at startup. The full ~/.omo can hold
# tens of GB of sessions and evidence that neither agent touches here. Copies use reflinks when
# the filesystem supports them, so they are cheap.
TEMPLATE=$WORK/template
rm -rf "$TEMPLATE"; mkdir -p "$TEMPLATE/.omo/agent"
for item in "$OMO_HOME"/agent/*.json extensions npm skills prompts omo-senpi runtime git; do
  case $item in /*) src=$item ;; *) src=$OMO_HOME/agent/$item ;; esac
  [ -e "$src" ] && cp -a --reflink=auto "$src" "$TEMPLATE/.omo/agent/"
done
for item in omo.json settings.json auth.json models.json trust.json skills npm extensions bin; do
  [ -e "$OMO_HOME/$item" ] && cp -a --reflink=auto "$OMO_HOME/$item" "$TEMPLATE/.omo/"
done

fresh_home() {
  rm -rf "$1"; cp -a --reflink=auto "$2" "$1"
  [ -d "$1/.bun" ] || cp -a --reflink=auto "$BUN_DIR" "$1/.bun"
  rm -rf "$1/.omo/agent/projects/bench-"* "$1/.omo/agent/sessions/--bench--" \
         "$1/.omo/agent/rpc/shards/"* "$1/.omo/agent/rpc-host-daemon/"* "$1/.omo/projects/bench-"*
}

run_one() {
  docker run --rm --name "membench-$3" --network host --user "$UIDGID" \
    -e HOME="$HOME" -e TERM=dumb \
    -e SENPI_CODING_AGENT_DIR="$HOME/.omo/agent" -e OMO_CODING_AGENT_DIR="$HOME/.omo/agent" \
    -e PATH="$NODE_DIR/bin:$MHC_DIR:$HOME/.bun/bin:/usr/local/bin:/usr/bin:/bin" \
    --tmpfs "$HOME:uid=$(id -u),gid=$(id -g),mode=755" \
    --tmpfs /tmp:mode=1777 --tmpfs "/bench:uid=$(id -u),mode=1777" \
    -v "$2/.omo:$HOME/.omo" -v "$2/.bun:$HOME/.bun" \
    -v "$MHC_DIR:$MHC_DIR:ro" -v "$NODE_DIR:$NODE_DIR:ro" \
    -v "$HERE/run-in-container.sh:/run-in-container.sh:ro" -v "$WORK/out:/out" \
    "$IMAGE" /run-in-container.sh "$1" "$3"
  echo "$3: $(cat "$WORK/out/$3.result")"
}

# Warm-up: first start migrates config and fills caches; its numbers are discarded, and the
# measured runs start from a copy of the warmed home.
for agent in $AGENTS; do
  fresh_home "$WORK/warm-$agent" "$TEMPLATE"
  run_one "$agent" "$WORK/warm-$agent" "warm-$agent"
done
for i in $(seq 1 "$RUNS"); do
  for agent in $AGENTS; do
    fresh_home "$WORK/run-$agent" "$WORK/warm-$agent"
    run_one "$agent" "$WORK/run-$agent" "$agent-$i"
  done
done

"$HERE/summarize.sh" "$WORK/out"
