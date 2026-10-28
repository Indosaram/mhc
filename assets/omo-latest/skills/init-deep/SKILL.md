---
name: init-deep
description: "Initializes a hierarchical AGENTS.md knowledge base for a project. Use when a repo needs its structure, commands, and conventions documented for agents."
argument-hint: "[--create-new] [--max-depth=N]"
---

# /init-deep

Build a tree of `AGENTS.md` files: one at the repo root, plus one in every subdirectory whose complexity score earns it. Agents reading them get the structure, commands, and conventions without re-exploring.

**Design goal:** the main session's context stays about the same size whether the repo is 50 KB or 500 MB. Size drives a map-reduce over a graph of worker nodes: cheap scanners → strong writers → one root writer → one verifier. The main session plans, launches, and reads verdicts. It never reads bulk data.

`<skill-dir>` below is the directory holding this SKILL.md. State lives in `.mhc/` at the repo root.

## Usage

```
/init-deep                      # Update mode: edit existing files, add new ones where warranted
/init-deep --create-new         # Read every existing AGENTS.md → delete them all → regenerate
/init-deep --max-depth=2        # Limit directory depth (default: 3)
```

## Who is reading this

- **Main session** (the user invoked `/init-deep`): run Phases 1–5 below.
- **Writer node / root-writer node** (a graph node that loaded this skill): skip to [Node instructions](#node-instructions). Scoring matrix, decision rules, templates, and the file-writing rule further down are your instructions. Do not run Phases 1–5.

---

## Core invariant: always reduce

1. **The main session never reads chunk reports or raw node output.** The only node output it reads is the verifier's verdict lines (`.mhc/init-deep/verify.txt`), plus at most one digest when a specific repair needs it.
2. **Context protection is structural, not judged at runtime.** Every stage has bounded fan-in: scanner reports ≤ ~5k tokens, digests ≤ ~2k tokens, ≤ 12 reports per writer, root writer reads digests only. Never pick a route from a guess about how much context is left.
3. **Repairs flow back through the graph.** Message or retry the node that owns the defective file. The main session editing files itself based on report contents is a defect.

---

## Workflow

<critical>
**Create the todo list first. Mark each item in_progress → completed as it actually happens.**
```
todowrite([
  { id: "size-route",      content: "Phase 1: measure source size, compute node counts, pick route", status: "pending", priority: "high" },
  { id: "map",             content: "Phase 2: map wave - scanners write chunk reports (inline: explore + code map)", status: "pending", priority: "high" },
  { id: "reduce",          content: "Phase 3: reduce wave - writers score subtrees, write AGENTS.md + digests", status: "pending", priority: "high" },
  { id: "root-verify",     content: "Phase 4: root writer + verifier, repair until all PASS", status: "pending", priority: "high" },
  { id: "snapshot-mode",   content: "Phase 5: local/committed mode, snapshot JSON, cleanup, report", status: "pending", priority: "medium" }
])
```
</critical>

| Phase | Who | DAG path | Inline path |
|---|---|---|---|
| 1 Size & route | main | script | script |
| 2 Map | scanner nodes (`quick`) | one per chunk | 2–4 `explore` agents + main-session code map |
| 3 Reduce | writer nodes (`unspecified-high`) | one per subtree | main session scores + writes |
| 4 Root & verify | root-writer + verify nodes | graph | main session writes root, runs verifier |
| 5 Snapshot & mode | main | same | same |

---

## Phase 1: Size & route (main session)

**Mark "size-route" in_progress.**

### 1.1 Measure in ONE code-execution step

Run the script; do not estimate by hand:

```bash
python3 <skill-dir>/scripts/size_route.py --max-depth=3 --max-nodes=64
```

Pass the user's `--max-depth`. Pass `--max-nodes` from mhc config `task.dag.max_nodes_per_run` if the user changed it (default 64).

It measures:
- `S`: total bytes of git-tracked source after excluding `node_modules`, `.git`, `dist`, `build`, `out`, `vendor`, `target`, `coverage`, lockfiles, minified files, and binary files;
- per-directory source bytes (the bin-packing input);
- maximum directory depth, capped at `--max-depth` (deeper directories fold into their ancestor at that depth);
- every existing `AGENTS.md` / `CLAUDE.md` path.

**Read the root `AGENTS.md` (or `CLAUDE.md`) now** if one exists. Read no other file at this point.

### 1.2 Formula (implemented by the script)

| Quantity | Value | Why |
|---|---|---|
| chunk | 400 KiB of source | ≈100k tokens; leaves room in a cheap worker's ~150k usable window for prompt + report |
| `N_quick` | `ceil(S / chunk)` | scanners. Chunks are bin-packed from **whole directories**; a directory bigger than one chunk is split along its child directories |
| `N_high` | `ceil(N_quick / 12)` | writers. One writer absorbs ~12 reports (~60k tokens) and still has room to open real code |

### 1.3 Route

| Condition | Route |
|---|---|
| `N_quick < 4` | **inline** (section "Inline path"). A graph costs more than it saves. |
| `N_quick > task.dag.max_nodes_per_run` (default 64) | **multi-run**: either raise the config value, or run one chained graph per top-level directory using the parallel orchestration skill's multi-run composition, then one final graph with the root writer + verifier. |
| otherwise | **dag**: re-run the script with `--write-plan`. |

`--write-plan` writes `.mhc/init-deep/plan.json` (chunk list: id, directories, bytes, owning writer; writer list: id, slug, owned directory roots) and creates `.mhc/init-deep/reports/` and `.mhc/init-deep/digests/`. Each writer owns a directory **subtree**; no directory is owned by two writers. The script prints a short summary; you never need to open `plan.json` in full — the launch cell reads it as data.

### 1.4 `--create-new` ordering

1. Existing files stay on disk through Phase 2 so scanners (or explore agents) read their still-true claims as input.
2. After the map wave finishes, delete every existing `AGENTS.md` in the repo.
3. Regenerate (Phases 3–4).

**Mark "size-route" completed.**

---

## Inline path (small repos, `N_quick < 4`)

No graph, no `.mhc/init-deep/reports/`. The main session does the whole job:

1. **Fire 2–4 `explore` subagents in the background, in parallel**, covering: structure, entry points, conventions, anti-patterns. Each must ground claims in structural search (ast-grep / `sg`) and file listing, and report deviations from the stack's standard patterns.
   ```
   task(subagent_type="explore", load_skills=[], run_in_background=true, description="init-deep: structure",
        prompt="Map real layout via file listing + lsp_symbols scope=workspace + sg. REPORT only deviations from standard layout. Max 40 lines.")
   task(subagent_type="explore", load_skills=[], run_in_background=true, description="init-deep: entry points",
        prompt="Find entry files; trace reach with lsp_symbols + lsp_find_references + sg import queries. REPORT non-standard organization. Max 40 lines.")
   task(subagent_type="explore", load_skills=[], run_in_background=true, description="init-deep: conventions",
        prompt="Read config files (lint, format, build, editor). REPORT project-specific rules that differ from defaults. Max 40 lines.")
   task(subagent_type="explore", load_skills=[], run_in_background=true, description="init-deep: anti-patterns",
        prompt="Find DO NOT / NEVER / ALWAYS / DEPRECATED comments and forbidden patterns. LIST them with file paths. Max 40 lines.")
   ```
2. **Meanwhile, run the code map yourself:**
   - `lsp_status`; then `lsp_symbols` scope="document" on each entry file (outline) and scope="workspace" by kind (symbol inventory);
   - `lsp_find_references` on the top exports (reference centrality);
   - ast-grep (`sg --pattern 'export $$$'`, `sg --pattern 'import $$$ from "$MOD"'`, and the language's call shapes) for export inventory and dependency edges.
   - If neither LSP nor ast-grep resolves, mark reference centrality **unmeasured** in the CODE MAP.
3. With `--create-new`: once the explore agents have reported, delete every existing `AGENTS.md` (their content is already in your context).
4. Score repo-wide with the [scoring matrix](#scoring-and-location), then write every file yourself with the [templates](#templates) and the file-writing rule.
5. Phase 4 inline: run `python3 <skill-dir>/scripts/verify.py --all`; fix each FAIL line and re-run until all PASS.
6. Run Phase 5 exactly as on the DAG path.

Node count on every path comes only from the formula. There is no fixed explore barrage and no extra-agent scaling table.

---

## Phase 2: Map wave — scanner nodes

**Mark "map" in_progress.**

Build and start the whole graph (Phases 2–4) in **one** code-execution step: an `eval` cell that reads `plan.json` and calls `tool.workflow`. Follow the parallel orchestration skill's planning reference for wave rules, the node prompt contract, and failure handling.

```js
// eval cell — compose the graph as code
const plan = JSON.parse(await read(".mhc/init-deep/plan.json"));
const CREATE_NEW = false;                                   // true when the user passed --create-new
// scannerPrompt(c), writerPrompt(w), ROOT_PROMPT, VERIFY_PROMPT: define in this same cell from the templates in this skill
const nodes = [];
for (const c of plan.chunks) nodes.push({
  id: c.id, category: "quick", load_skills: [], description: `init-deep scan ${c.id}`,
  prompt: scannerPrompt(c),                                  // template below, filled per chunk
});
const gate = [];
if (CREATE_NEW) {                                           // delete old files only after every scanner read them
  nodes.push({ id: "cleanup", category: "quick", load_skills: [], dependsOn: plan.chunks.map(c => c.id),
    description: "init-deep delete old AGENTS.md",
    prompt: "Delete every AGENTS.md in this repo except under node_modules/ and .git/. Touch nothing else. Final message: the deleted paths." });
  gate.push("cleanup");
}
for (const w of plan.writers) nodes.push({
  id: w.id, category: "unspecified-high", load_skills: ["init-deep"], dependsOn: [...w.chunks, ...gate],
  description: `init-deep write ${w.slug}`, prompt: writerPrompt(w),     // template in Phase 3
});
nodes.push({ id: "root-writer", category: "unspecified-high", load_skills: ["init-deep"],
  dependsOn: plan.writers.map(w => w.id), description: "init-deep root", prompt: ROOT_PROMPT });
nodes.push({ id: "verify", category: "quick", load_skills: [], dependsOn: ["root-writer"],
  description: "init-deep verify", prompt: VERIFY_PROMPT });
const res = await tool.workflow({ action: "start",
  definition: { key: `init-deep-${plan.S_bytes}-${plan.N_quick}`, name: "init-deep", nodes } });
globalThis.initDeepRun = res.details?.run_id;
print(res.text);                                             // one-line summary only, never the snapshot
```

`dependsOn` is ordering only: no upstream output is pasted into a downstream prompt, so every prompt must stand alone and name its input files explicitly.

Scanners extract facts only. They make no judgments, load no skills, and never write `AGENTS.md`.

### Scanner prompt (rigid template — fill the bracketed fields, change nothing else)

```
TASK
Extract facts about chunk {chunk-id} of this repository. Do not judge, recommend, or write AGENTS.md.

DIRECTORIES (your whole scope)
{one path per line; "(own files only)" after a path means: files directly in it, not subdirectories}

STEPS (in order, for those directories only)
1. INVENTORY: per directory: file count, LOC, languages, entry files.
2. EXPORTS: public symbols imported by other code. Find them with lsp_symbols and
   ast-grep (sg) import/call queries. Never guess from file names.
3. CONVENTIONS: configs, naming, and layout that deviate from the stack's defaults.
4. ANTI-PATTERNS: comments containing DO NOT / NEVER / ALWAYS / DEPRECATED; forbidden patterns.
5. HOTSPOTS: files over 500 lines, heavily referenced symbols (lsp_find_references), complexity clusters.
6. COMMANDS: build / test / dev commands that touch these directories.
If an AGENTS.md exists inside your directories, carry its still-true claims into the matching sections.

DELIVERABLE
Exactly one file: .mhc/init-deep/reports/{chunk-id}.md, at most ~5k tokens:
  # Chunk {chunk-id}: {first directory} (+N more)
  ## INVENTORY
  ## EXPORTS
  ## CONVENTIONS
  ## ANTI-PATTERNS
  ## HOTSPOTS
  ## COMMANDS
Telegraphic bullets with file paths. An empty section contains the single word: none

SCOPE
Read only the directories above. Write only your report file.

VERIFY
The report exists and contains all six section headers.

STOP
Stop as soon as VERIFY passes. Final message: "DONE {chunk-id}".
```

**Do not read scanner output.** Mark "map" completed when the scanner nodes have settled (the session is woken as nodes complete). With `--create-new`, the `cleanup` node in the cell above deletes the old files after every scanner has read them and before any writer starts (see 1.4).

---

## Phase 3: Reduce wave — writer nodes

**Mark "reduce" in_progress.**

One writer node per subtree: category `unspecified-high`, `dependsOn` its chunks' scanner nodes, `load_skills: ["init-deep"]`. Loading this skill gives the writer the scoring matrix, decision rules, templates, and file-writing rule; its prompt only adds the assignment.

### Writer prompt (fill per writer)

```
TASK
You are init-deep writer {writer-id}. Follow the "Node instructions → Writer node" section of the init-deep skill.

SUBTREE (you own exactly these; "(own files only)" means that directory itself but not its subdirectories)
{owned roots}

INPUTS
Chunk reports: .mhc/init-deep/reports/{chunk-ids}.md

DELIVERABLES
- AGENTS.md files at the locations you choose inside your subtree
- Digest: .mhc/init-deep/digests/{writer-slug}.md

SCOPE
Write only inside your subtree and your digest. Never write the root ./AGENTS.md.
```

Wait for the run with `tool.workflow({ action: "wait", run_id })` (detached by default; you are woken as nodes settle). Do not print snapshot node outputs: `snapshot` carries every node's output, so if you call it, print only node states.

**Mark "reduce" completed** when all writer nodes have settled.

---

## Phase 4: Root writer and verifier

**Mark "root-verify" in_progress.**

- **Root writer** (`root-writer`): `unspecified-high`, loads this skill, depends on all writers. Inputs: ONLY `.mhc/init-deep/digests/` plus the existing root `AGENTS.md` if any. It never opens chunk reports. Its prompt:
  ```
  You are the init-deep root writer. Follow "Node instructions → Root-writer node" in the init-deep skill.
  Inputs: .mhc/init-deep/digests/*.md and ./AGENTS.md (if it exists). Deliverable: ./AGENTS.md. Write nothing else.
  ```
- **Verifier** (`verify`): `quick`, depends on the root writer. Its prompt (the main session substitutes the absolute skill directory for `<skill-dir>`; same for every node prompt):
  ```
  Run: python3 <skill-dir>/scripts/verify.py
  It checks: every path declared in the digests exists; root AGENTS.md is 50-150 lines; each subdirectory
  AGENTS.md is 30-80 lines and states why it exists; no child repeats a block from an ancestor.
  It prints one line per file - "PASS <path>" or "FAIL <path>: <reason>" - and writes them to
  .mhc/init-deep/verify.txt. Your final message is that output verbatim. Do not edit any file.
  ```

### Repair loop (main session)

1. Read **only** `.mhc/init-deep/verify.txt` (or the verify node's output, printed alone from the snapshot).
2. For each `FAIL <path>: <reason>`: find the owner with `python3 <skill-dir>/scripts/size_route.py --owner <path>` (prints a writer id or `root-writer`). Then
   `tool.workflow({ action: "send", run_id, node_id: <owner>, message: "Fix <path>: <reason>. Edit only that file (and your digest if a path changed)." })`,
   or `tool.workflow({ action: "retry", run_id, node_ids: [<owner>], reason: "<path>: <reason>" })` if the node failed outright.
3. Re-run verification: `tool.workflow({ action: "retry", run_id, node_ids: ["verify"] })`, then read `verify.txt` again.
4. Loop until every line is `PASS`. At most one digest read per repair, and only when the FAIL reason cannot be routed without it.

**Mark "root-verify" completed.**

### Fallback: `workflow` tool unavailable

If `tool.workflow` is not exposed in this session, keep every file contract above (same prompts, report and digest paths, verify script) and run the waves as batches of `task` calls:

1. Scanners: `task(category="quick", load_skills=[], run_in_background=true, prompt=<scanner prompt>)`, launched in batches (e.g. 8 at a time, or the session's background-task cap). Wait for each batch with `task_status`; do **not** call `task_output` on scanner tasks.
2. Writers: same, with `category="unspecified-high"`, `load_skills=["init-deep"]`, once every scanner is done.
3. Root writer, then verifier, each as one `task`. The verifier writes `verify.txt`; the main session reads only that.
4. Repairs: `task_send` (or a fresh `task` with the writer prompt + the FAIL line) to the owning writer, then re-run the verifier task.

---

## Phase 5: Snapshot & mode (main session)

**Mark "snapshot-mode" in_progress.**

### 5.1 Ask for the mode

Ask the user once: keep the generated guidance **local** (private to this checkout) or **committed** (lands through a reviewed PR)? Record the answer as `USER_MODE_CHOICE`.
- An explicit answer always wins.
- No explicit answer: `committed` if root `AGENTS.md` is tracked by git, else `local`.
- Explicit `committed` applies even if `AGENTS.md` is untracked.
- Explicit `local` applies even if `AGENTS.md` is tracked — but git's exclude file cannot hide changes to tracked files, so local mode on a tracked file is informational only and the file stays visible to git. Tell the user.

### 5.2 Write the snapshot

```bash
python3 <skill-dir>/scripts/snapshot.py "$USER_MODE_CHOICE"   # omit the argument when the user gave no explicit answer
```

Writes `.mhc/init-deep.json` (creating `.mhc/` if needed), one JSON object:

| Field | Value |
|---|---|
| `commitSha` | full SHA of `HEAD` |
| `fileCount` | git-tracked files, counted from NUL-separated `git ls-files -z` (newlines in names and stream chunk boundaries cannot skew it) |
| `loc` | total lines over tracked `ts tsx js jsx py go rs java kt swift rb php c cpp cs scala lua ex exs zig dart` files; `0` if none or on error |
| `timestamp` | current Unix time in **milliseconds** (13 digits) |
| `mode` | `local` or `committed` |

### 5.3 Apply the mode

- **local**: `sh <skill-dir>/scripts/local_mode.sh add`. Resolves the exclude file with `git rev-parse --git-path info/exclude` (worktree-safe), creates its directory if needed, and appends once a block delimited by `# >>> mhc init-deep local (managed) >>>` / `# <<< mhc init-deep local (managed) <<<`, containing a do-not-edit comment, `/.mhc/init-deep.json` (anchored at repo root) and `AGENTS.md` (unanchored: nested copies too). Idempotent: skips if the begin marker exists. Never alters user lines.
- **committed**: hand the change to the PR workflow skill (`work-with-pr`) so it lands via a reviewed PR.
- **Switching local → committed**: `sh <skill-dir>/scripts/local_mode.sh remove` (deletes marker to marker inclusive via `sed … > tmp && mv`, portable across GNU/BSD, no backup file), then the PR workflow skill.

### 5.4 List, clean up, report

1. List all nested `AGENTS.md` files, excluding `node_modules`, `.git`, `dist`, `build`:
   `find . -name AGENTS.md -not -path '*/node_modules/*' -not -path '*/.git/*' -not -path '*/dist/*' -not -path '*/build/*'`
2. Delete the temporary work directory: `rm -rf .mhc/init-deep/`. `.mhc/init-deep.json` is the only lasting artifact.
3. Print the final report.

**Mark "snapshot-mode" completed.**

---

## Final Report

```
=== init-deep Complete ===

Mode: {update | create-new} · Guidance: {local | committed}
Sizing: S={S_MB} MB · {N_quick} scanners · {N_high} writers · path={dag | inline | multi-run}

Files:
  [OK] ./AGENTS.md (root, {N} lines)
  [OK] ./src/hooks/AGENTS.md ({N} lines)

Dirs Analyzed: {N}
AGENTS.md Created: {N}
AGENTS.md Updated: {N}

Hierarchy:
  ./AGENTS.md
  └── src/hooks/AGENTS.md

Cleanup: removed .mhc/init-deep/ (reports, digests, plan); kept .mhc/init-deep.json
```

---

## Node instructions

### Writer node

1. **Read your chunk reports.** Treat them as claims. Where reports conflict or look thin, spot-check the actual code (`lsp_symbols`, `lsp_find_references`, `sg`, targeted reads).
2. **Score every directory in your subtree** with the [scoring matrix](#scoring-and-location) and pick locations with the decision rules. Only directories within `--max-depth`.
3. **Write each chosen `AGENTS.md`** with the subdirectory template and the file-writing rule: 30–80 lines, never repeating content from any ancestor `AGENTS.md` (the root is written after you; keep repo-wide facts out of your files and put them in your digest instead).
4. **Write your digest** at `.mhc/init-deep/digests/<your-slug>.md`, ≤ ~2k tokens:
   ```
   # Digest {writer-id}: {subtree roots}
   ## LOCATIONS
   - src/hooks/AGENTS.md | score 18 | lifecycle hooks; plugin entry points
   - src/api/AGENTS.md | score 12 | HTTP layer, distinct domain
   ## FOR ROOT
   - {cross-subtree facts the root needs: shared commands, repo-wide conventions, top symbols with refs}
   ```
   One `LOCATIONS` line per file you wrote, in exactly that `path | score N | role` shape (the verifier parses it).
5. **Scope:** write only inside your subtree plus your digest. Never touch the root `AGENTS.md`.
6. **Self-check, then stop:** every location you chose exists on disk and is 30–80 lines; the digest exists.

### Root-writer node

1. Read ONLY the files in `.mhc/init-deep/digests/` and the existing root `AGENTS.md` (if any). Do not open chunk reports. Spot-check code only to confirm a command or a top symbol.
2. Write `./AGENTS.md` with the root template and the file-writing rule, 50–150 lines. Link down to subdirectory files via WHERE TO LOOK; do not copy their content.
3. Self-check: file exists, 50–150 lines. Stop.

---

## Scoring and location

Applied by each writer within its own subtree; on the inline path, by the main session repo-wide.

### Scoring matrix

| Factor | Weight | High Threshold | Source |
|--------|--------|----------------|--------|
| File count | 3x | >20 | bash |
| Subdir count | 2x | >5 | bash |
| Code ratio | 2x | >70% | bash |
| Unique patterns | 1x | Has own config | explore / scanner report |
| Module boundary | 2x | Has index.ts/__init__.py | bash |
| Symbol density | 2x | >30 symbols | LSP / ast-grep (`sg`) |
| Export count | 2x | >10 exports | LSP / ast-grep (`sg`) |
| Reference centrality | 3x | >20 refs | LSP / ast-grep (`sg`) |

### Decision rules

| Score | Action |
|-------|--------|
| **Root (.)** | ALWAYS create |
| **>15** | Create AGENTS.md |
| **8-15** | Create if distinct domain |
| **<8** | Skip (parent covers) |

### Output

```
AGENTS_LOCATIONS = [
  { path: ".", type: "root" },
  { path: "src/hooks", score: 18, reason: "high complexity" },
  { path: "src/api", score: 12, reason: "distinct domain" }
]
```

---

## Templates

<critical>
**File Writing Rule**: If AGENTS.md already exists at the target path → use the `edit` tool. If it does NOT exist → use the `write` tool.
NEVER use a whole-file write to overwrite an existing file. ALWAYS check existence first (read it, or check the discovery results).
</critical>

### Root AGENTS.md (full treatment)

~~~markdown
# PROJECT KNOWLEDGE BASE

**Generated:** {TIMESTAMP}
**Commit:** {SHORT_SHA}
**Branch:** {BRANCH}

## OVERVIEW
{1-2 sentences: what + core stack}

## STRUCTURE
```
{root}/
├── {dir}/    # {non-obvious purpose only}
└── {entry}
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|

## CODE MAP
{From LSP/ast-grep - skip only if neither exists or project <10 files}

| Symbol | Type | Location | Refs | Role |
|--------|------|----------|------|------|

## CONVENTIONS
{ONLY deviations from standard}

## ANTI-PATTERNS (THIS PROJECT)
{Explicitly forbidden here}

## UNIQUE STYLES
{Project-specific}

## COMMANDS
```bash
{dev/test/build}
```

## NOTES
{Gotchas}
~~~

**Quality gates**: 50–150 lines, no generic advice, no obvious info.

### Subdirectory AGENTS.md

~~~markdown
# {path}

{OVERVIEW: one line}
Own file: score {N} ({top factors}) | distinct domain: {what makes it separate}

## STRUCTURE            <- only if >5 subdirectories
## WHERE TO LOOK
| Task | Location | Notes |
## CONVENTIONS          <- only where they differ from the parent
## ANTI-PATTERNS
~~~

- 30–80 lines. NEVER repeat parent content.
- The "Own file" line is required: it states why this directory earned its own file (score and/or distinct domain).
- Written by writer nodes (DAG path) or the main session (inline path).

---

## Anti-Patterns

- **Sequential execution**: MUST fan out. DAG: the map wave runs all scanners in parallel. Inline: explore agents + LSP + ast-grep run concurrently.
- **Ignoring existing**: ALWAYS read existing files first, even with `--create-new`.
- **Over-documenting**: Not every dir needs AGENTS.md.
- **Redundancy**: Child never repeats parent.
- **Generic content**: Remove anything that applies to ALL projects.
- **Verbose style**: Telegraphic or die.
- **Pulling node output into the main session**: never read reports or node outputs; repairs go through the graph (send / retry the owning node).
- **Wrong node granularity**: nodes own batches. Never one node per file or per source; the formula sets the count.
- **Free-form scanner prompts**: scanner prompts are the numbered extraction template above, never free-form.
