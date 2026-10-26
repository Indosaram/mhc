#!/usr/bin/env bash
# Runs inside one benchmark container: one agent CLI, one fixed workflow DAG, one result.
# Usage: run-in-container.sh <mhc|omo> <tag>
# The container's own cgroup is /sys/fs/cgroup, so every process the agent spawns is counted.
# Writes /out/<tag>.samples (every 0.2s: epoch memory.current anon file nprocs),
# /out/<tag>.out (agent stdout+stderr) and /out/<tag>.result (one key=value line).
set -u
BIN=$1; TAG=$2; W=/bench
mkdir -p "$W/work"; cd "$W" || exit 1
for f in a b c; do seq 1 40 | sed "s/^/$f line /" > "$W/work/$f.txt"; done

(
  while :; do
    printf '%s %s %s%s\n' "$(date +%s.%N)" "$(cat /sys/fs/cgroup/memory.current)" \
      "$(awk '$1=="anon"||$1=="file"{printf "%s ",$2}' /sys/fs/cgroup/memory.stat)" \
      "$(wc -l < /sys/fs/cgroup/cgroup.procs)" >> "/out/$TAG.samples"
    sleep 0.2
  done
) &
SAMPLER=$!

PROMPT="workflow 도구로 아래 고정 DAG를 정확히 한 번 실행해. 노드를 추가하거나 바꾸지 마.
- a: $W/work/a.txt 를 읽고 한 문장으로 요약 (category quick)
- b: $W/work/b.txt 를 읽고 한 문장으로 요약 (category quick)
- c: $W/work/c.txt 를 읽고 한 문장으로 요약 (category quick)
- join (dependsOn a,b,c): 세 요약을 한 줄로 합쳐 $W/work/out.txt 에 써 (category quick)
완료되면 \"DONE\" 한 줄만 출력해."

start=$(date +%s)
timeout -k 10 900 "$BIN" -p "$PROMPT" > "/out/$TAG.out" 2>&1
rc=$?
end=$(date +%s)
kill "$SAMPLER"

ok=no; [ -s "$W/work/out.txt" ] && ok=yes
echo "rc=$rc secs=$((end - start)) peak=$(cat /sys/fs/cgroup/memory.peak) out_ok=$ok" > "/out/$TAG.result"
