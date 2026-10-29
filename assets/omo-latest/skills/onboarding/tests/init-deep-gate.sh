#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Indo Yoon
#
# Checks scripts/init-deep-gate.mjs: the four reasons, boundary values, exclusions,
# depth limit, ancestor coverage, and that its constants equal the advisor's.
# Optional: ADVISOR_CONSTANTS=<path to constants.rs> to cross-check against mhc source.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$HERE/../scripts/init-deep-gate.mjs"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

field() { node -e 'const o=JSON.parse(process.argv[1]);process.stdout.write(JSON.stringify(new Function("o","return "+process.argv[2])(o)))' "$1" "$2"; }
expect() { local out; out="$(node "$GATE" "$1")"; local got; got="$(field "$out" "$2")"; [[ "$got" == "$3" ]] || fail "$4: $2=$got expected $3 ($out)"; }

newrepo() { local d="$TMP/$1"; mkdir -p "$d"; git -C "$d" init -q; echo "$d"; }
srcfiles() { mkdir -p "$1"; for ((i=0;i<$2;i++)); do printf 'a\nb' > "$1/f$i.${3:-ts}"; done; }

export GIT_CEILING_DIRECTORIES="$TMP"

# not-git
mkdir -p "$TMP/plain"
expect "$TMP/plain" 'o.reason' '"not-git"' "outside repo"
expect "$TMP/plain" 'o.eligible' 'false' "outside repo"

# no-candidates: tiny repo; 7 files is below the threshold; root files never count
R="$(newrepo tiny)"; srcfiles "$R/src" 7; srcfiles "$R" 20
expect "$R" 'o.reason' '"no-candidates"' "tiny repo"

# eligible: one dir with exactly 8 files, no AGENTS.md
R="$(newrepo elig)"; srcfiles "$R/src" 8
expect "$R" 'o.reason' '"eligible"' "8 files"
expect "$R" 'o.missingRatio' '1' "no AGENTS.md => 1.0"
expect "$R/src" 'o.repoRoot' "\"$R\"" "evaluated from a subdirectory"

# LOC rule: one file with 500 lines (499 newlines => 500 pieces) qualifies; 499 lines does not
R="$(newrepo loc)"; mkdir -p "$R/a" "$R/b"
node -e 'require("fs").writeFileSync(process.argv[1],"x\n".repeat(499).slice(0,-1)+"\nx")' "$R/a/big.py"
node -e 'require("fs").writeFileSync(process.argv[1],"x\n".repeat(498)+"x")' "$R/b/small.py"
expect "$R" 'o.candidates' '1' "500-line boundary"
expect "$R" 'o.candidateDirs[0].path' '"a"' "500-line boundary"

# non-source extensions ignored; excluded dirs and depth > 3 ignored
R="$(newrepo excl)"; srcfiles "$R/docs" 20 md; srcfiles "$R/node_modules/x" 20; srcfiles "$R/pkg/target" 20
srcfiles "$R/a/b/c/d" 20; srcfiles "$R/a/b/c" 8
expect "$R" 'o.candidates' '1' "exclusions/depth"
expect "$R" 'o.candidateDirs[0].path' '"a/b/c"' "depth 3 allowed, depth 4 not"

# covered: root AGENTS.md covers every candidate
R="$(newrepo cov)"; srcfiles "$R/src" 8; srcfiles "$R/lib/core" 9; echo "# x" > "$R/AGENTS.md"
expect "$R" 'o.reason' '"covered"' "root AGENTS.md"
expect "$R" 'o.covered' '2' "root AGENTS.md"

# ratio boundary: 2 candidates, 1 covered by its own AGENTS.md => 0.5 => eligible
R="$(newrepo half)"; srcfiles "$R/a" 8; srcfiles "$R/b" 8; echo x > "$R/a/AGENTS.md"
expect "$R" 'o.missingRatio' '0.5' "half"
expect "$R" 'o.reason' '"eligible"' "0.5 meets threshold"
# 3 candidates, 2 covered (ancestor coverage) => 1/3 => covered
srcfiles "$R/a/inner" 8
expect "$R" 'o.reason' '"covered"' "ancestor AGENTS.md covers a/inner"

# usage errors
set +e
node "$GATE" a b >/dev/null 2>&1; [[ $? -eq 2 ]] || fail "two args should exit 2"
node "$GATE" "$TMP/missing" >/dev/null 2>&1; [[ $? -eq 2 ]] || fail "missing dir should exit 2"
set -e

# constants equal the advisor's
CONSTS="$(node -e '
const src=require("fs").readFileSync(process.argv[1],"utf8");
const num=(n)=>Number(src.match(new RegExp("export const "+n+" = ([0-9.]+)"))[1]);
const arr=(n)=>JSON.parse(src.match(new RegExp("export const "+n+" = (\\[[^\\]]*\\])"))[1].replace(/\x27/g,"\"").replace(/\s+/g," "));
console.log(JSON.stringify({d:num("CANDIDATE_MAX_DEPTH"),f:num("CANDIDATE_MIN_FILES"),l:num("CANDIDATE_MIN_LOC"),r:num("MISSING_COVERAGE_RATIO_THRESHOLD"),e:arr("SOURCE_EXTENSIONS"),x:arr("EXCLUDED_DIR_NAMES")}));
' "$GATE")"
EXPECTED='{"d":3,"f":8,"l":500,"r":0.5,"e":["ts","tsx","js","jsx","py","go","rs","java","kt","swift","rb","php","c","cpp","cs","scala","lua","ex","exs","zig","dart"],"x":["node_modules",".git","dist","build","vendor",".next","__pycache__",".venv","target","coverage","third_party"]}'
[[ "$CONSTS" == "$EXPECTED" ]] || fail "gate constants differ from spec §3.6: $CONSTS"
if [[ -n "${ADVISOR_CONSTANTS:-}" && -f "$ADVISOR_CONSTANTS" ]]; then
  RUST="$(node -e '
const s=require("fs").readFileSync(process.argv[1],"utf8");
const num=(n)=>Number(s.match(new RegExp("pub const "+n+": [a-z0-9]+ = ([0-9.]+)"))[1]);
const arr=(n)=>[...s.match(new RegExp("pub const "+n+": &\\[&str\\] = &\\[([^\\]]*)\\]"))[1].matchAll(/"([^"]+)"/g)].map(m=>m[1]);
console.log(JSON.stringify({d:num("CANDIDATE_MAX_DEPTH"),f:num("CANDIDATE_MIN_FILES"),l:num("CANDIDATE_MIN_LOC"),r:num("MISSING_COVERAGE_RATIO_THRESHOLD"),e:arr("SOURCE_EXTENSIONS"),x:arr("EXCLUDED_DIR_NAMES")}));
' "$ADVISOR_CONSTANTS")"
  [[ "$RUST" == "$CONSTS" ]] || fail "gate constants differ from $ADVISOR_CONSTANTS: $RUST"
  echo "  constants match $ADVISOR_CONSTANTS"
fi

echo "OK init-deep-gate: not-git, no-candidates, covered, eligible, boundaries, exclusions, depth, constants"
