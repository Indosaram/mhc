#!/usr/bin/env bash
# Summarize benchmark results: one row per measured run (warm-up runs are skipped).
# Usage: tools/membench/summarize.sh <out dir>
# Columns, in MB of the run's container cgroup:
#   peak       memory.peak (anon + page cache + kernel), the kernel's own high-water mark
#   anon_peak  highest sampled anon (heap, stacks, JIT; memory the processes actually allocated)
#   anon_avg   mean sampled anon over the run
#   file_peak  highest sampled page cache (binaries, node_modules, files read)
set -euo pipefail
OUT=${1:?usage: summarize.sh <out dir>}
printf '%-8s %4s %5s %7s %9s %8s %9s %5s %3s\n' run rc secs peak anon_peak anon_avg file_peak procs ok
for result in "$OUT"/*.result; do
  tag=$(basename "$result" .result)
  case $tag in warm-*) continue ;; esac
  read -r rc secs peak ok < <(awk '{for(i=1;i<=NF;i++){split($i,kv,"=");v[kv[1]]=kv[2]}} END{print v["rc"],v["secs"],v["peak"],v["out_ok"]}' "$result")
  awk -v t="$tag" -v rc="$rc" -v s="$secs" -v pk="$peak" -v ok="$ok" '
    { a=$3/1048576; f=$4/1048576; if(a>ma)ma=a; if(f>mf)mf=f; sa+=a; if($5>p)p=$5; n++ }
    END { printf "%-8s %4s %5s %7.0f %9.0f %8.0f %9.0f %5d %3s\n", t, rc, s, pk/1048576, ma, sa/n, mf, p, ok }
  ' "$OUT/$tag.samples"
done
