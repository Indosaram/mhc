#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Indo Yoon
#
# Fixture test for scripts/savings-estimate.mjs.
#   Vector A: the independent test vector of the clean-room spec (§6.1), online + offline.
#   Vector B: implementer's own data (per-message identity over model_change, no timestamp re-sort,
#             nested directory expansion).
#   CLI: usage errors exit 2, unreadable input exits 1, empty dir exits 0 with no data.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CALC="$HERE/../scripts/savings-estimate.mjs"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

# check <json-file> <jq-expression> <expected> <label>
check() {
  local got
  got="$(node -e '
    const o = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    const v = new Function("o", "return " + process.argv[2])(o);
    process.stdout.write(JSON.stringify(v));
  ' "$1" "$2")"
  [[ "$got" == "$3" ]] || fail "$4: $2 = $got, expected $3"
}

# ---------- Vector A ----------
A="$TMP/a"; mkdir -p "$A/logs"
cat > "$A/catalog.json" <<'JSON'
{
  "p-one":  {"models": {"m-x":      {"cost": {"input": 5, "output": 25, "cache_read": 0.5, "cache_write": 6.25}}}},
  "b-prov": {"models": {"m-shared": {"cost": {"input": 2, "output": 8,  "cache_read": 0.2, "cache_write": 2.5}}}},
  "a-prov": {"models": {"m-shared": {"cost": {"input": 1, "output": 4,  "cache_read": 0.1, "cache_write": 1.25}}}},
  "p-two":  {"models": {"m-nocw":   {"cost": {"input": 1, "output": 2,  "cache_read": 0.1}}}}
}
JSON
cat > "$A/logs/2026-01-01-a.jsonl" <<'JSONL'
{"type":"session","version":3,"id":"sa","timestamp":"2026-01-01T09:00:00.000Z","cwd":"/w"}
{"type":"message","timestamp":"2026-01-01T09:00:01.000Z","message":{"role":"assistant","usage":{"input":10000,"output":1000,"cacheRead":30000,"cacheWrite":0}}}
{"type":"model_change","provider":"p-one","modelId":"m-x","timestamp":"2026-01-01T09:00:02.000Z"}
{"type":"message","timestamp":"2026-01-01T09:00:03.000Z","message":{"role":"assistant","provider":"unknown-prov","model":"m-shared","usage":{"input":20000,"output":2000,"cacheRead":20000,"cacheWrite":10000}}}
{"type":"message","timestamp":"2026-01-01T09:00:04.000Z","message":{"role":"assistant","usage":{"input":0,"output":500,"cacheRead":10000,"cacheWrite":0}}}
{"type":"message","timestamp":"garbage","message":{"role":"assistant","provider":"p-one","model":"m-x","usage":{"input":9000000,"output":9000000,"cacheRead":9000000,"cacheWrite":9000000}}}
JSONL
cat > "$A/logs/2026-01-02-b.jsonl" <<'JSONL'
{"type":"model_change","provider":"p-two","modelId":"m-nocw","timestamp":"2026-01-02T09:00:00.000Z"}
{"type":"message","timestamp":"2026-01-02T09:00:01.000Z","message":{"role":"assistant","usage":{"input":1000,"output":100,"cacheRead":1000,"cacheWrite":100}}}
JSONL
cat > "$A/logs/2026-01-03-c.jsonl" <<'JSONL'
{"type":"session","version":3,"id":"sc","timestamp":"2026-01-03T09:00:00.000Z","cwd":"/w"}
{"type":"message","timestamp":"2026-01-03T09:00:01.000Z","message":{"role":"assistant","usage":{"input":5000,"output":100,"cacheRead":0,"cacheWrite":0}}}
JSONL

node "$CALC" --models "$A/catalog.json" "$A/logs" > "$A/online.json" || fail "vector A online run exited $?"
check "$A/online.json" 'o.input'                30000 "A online"
check "$A/online.json" 'o.cacheRead'            60000 "A online"
check "$A/online.json" 'o.cacheWrite'           10000 "A online"
check "$A/online.json" 'o.output'               3500  "A online"
check "$A/online.json" 'o.cacheRatePct'         60    "A online"
check "$A/online.json" 'o.actualCost'           0.15  "A online"
check "$A/online.json" 'o.baselineCost'         0.35  "A online"
check "$A/online.json" 'o.savings'              0.2   "A online"
check "$A/online.json" 'o.insufficientSessions' 1     "A online"
check "$A/online.json" 'o.zeroInputSessions'    1     "A online"
check "$A/online.json" 'o.malformedSkipped'     1     "A online"
check "$A/online.json" 'o.byModel.find(r=>r.model==="m-shared").pricedAs' '"a-prov"' "A composite-miss fallback"
check "$A/online.json" 'o.byModel.find(r=>r.model==="m-shared").provider' '"unknown-prov"' "A per-message identity"
# pre-change usage (10000/30000) + post-change usage (0/10000) both land on p-one/m-x
check "$A/online.json" 'o.byModel.find(r=>r.model==="m-x").input'     10000 "A pre-change attribution"
check "$A/online.json" 'o.byModel.find(r=>r.model==="m-x").cacheRead' 40000 "A model_change attribution"
check "$A/online.json" 'o.byModel.length' 2 "A no p-two row (insufficient session excluded)"

node "$CALC" --offline --models "$A/catalog.json" "$A/logs" > "$A/offline.json" || fail "vector A offline run exited $?"
check "$A/offline.json" 'o.insufficientSessions' 2    "A offline"
check "$A/offline.json" 'o.zeroInputSessions'    1    "A offline"
check "$A/offline.json" 'o.cacheRatePct'         null "A offline"
check "$A/offline.json" 'o.actualCost'           0    "A offline"

# unreachable catalog behaves like offline
node "$CALC" --models "$A/does-not-exist.json" "$A/logs" > "$A/nocat.json" || fail "missing-catalog run exited $?"
check "$A/nocat.json" 'o.insufficientSessions' 2 "A unreachable catalog"

# ---------- Vector B (own data) ----------
B="$TMP/b"; mkdir -p "$B/sessions/--w--"
cat > "$B/catalog.json" <<'JSON'
{
  "q-b": {"models": {"m-2": {"cost": {"input": 0.5, "output": 1.5, "cache_read": 0.05, "cache_write": 0.625}}}},
  "q-a": {"models": {"m-1": {"cost": {"input": 3,   "output": 15,  "cache_read": 0.3,  "cache_write": 3.75}}}}
}
JSON
# The last message's timestamp is EARLIER than the model_change before it: line order must win.
cat > "$B/sessions/--w--/s1.jsonl" <<'JSONL'
{"type":"session","version":3,"id":"s1","timestamp":"2026-02-01T10:00:00.000Z","cwd":"/w"}
{"type":"model_change","provider":"q-b","modelId":"m-2","timestamp":"2026-02-01T10:00:01.000Z"}
{"type":"message","timestamp":"2026-02-01T10:00:01.000Z","message":{"role":"assistant","provider":"q-a","model":"m-1","usage":{"input":100000,"output":10000,"cacheRead":400000,"cacheWrite":50000}}}
{"type":"message","timestamp":"2026-02-01T10:00:02.000Z","message":{"role":"assistant","usage":{"input":200000,"output":20000,"cacheRead":0,"cacheWrite":0}}}
{"type":"model_change","provider":"q-a","modelId":"m-1","timestamp":"2026-02-01T10:00:09.000Z"}
{"type":"message","timestamp":"2026-02-01T10:00:03.000Z","message":{"role":"assistant","usage":{"input":0,"output":1000,"cacheRead":100000,"cacheWrite":0}}}
JSONL
node "$CALC" --models "$B/catalog.json" "$B/sessions" > "$B/out.json" || fail "vector B run exited $?"
check "$B/out.json" 'o.input'        300000 "B"
check "$B/out.json" 'o.cacheRead'    500000 "B"
check "$B/out.json" 'o.cacheWrite'   50000  "B"
check "$B/out.json" 'o.output'       31000  "B"
check "$B/out.json" 'o.cacheRatePct' 58.82  "B"
check "$B/out.json" 'o.actualCost'   0.93   "B"
check "$B/out.json" 'o.savings'      1.35   "B"
check "$B/out.json" 'o.baselineCost' 2.28   "B"
check "$B/out.json" 'o.insufficientSessions+o.zeroInputSessions+o.malformedSkipped' 0 "B"
check "$B/out.json" 'o.byModel.find(r=>r.provider==="q-b").input' 200000 "B model_change attribution"
check "$B/out.json" 'o.byModel.find(r=>r.provider==="q-a").pricedAs' '"q-a"' "B exact price"

# ---------- CLI contract ----------
set +e
node "$CALC" >/dev/null 2>&1;                         [[ $? -eq 2 ]] || fail "no inputs should exit 2"
node "$CALC" --bogus "$A/logs" >/dev/null 2>&1;       [[ $? -eq 2 ]] || fail "unknown option should exit 2"
node "$CALC" --models >/dev/null 2>&1;                [[ $? -eq 2 ]] || fail "--models without value should exit 2"
node "$CALC" --offline "$TMP/nope.jsonl" >/dev/null 2>&1; [[ $? -eq 1 ]] || fail "unreadable input should exit 1"
set -e
mkdir -p "$TMP/empty"
node "$CALC" --offline "$TMP/empty" > "$TMP/empty.json" || fail "empty dir should exit 0"
check "$TMP/empty.json" 'o.cacheRatePct' null "empty"
check "$TMP/empty.json" 'o.sessions' 0 "empty"

echo "OK savings-fixture: vector A online (cost 0.15, savings 0.2, baseline 0.35, cache 60%) + offline, vector B, CLI exits"
