---
name: dag-library
description: Save a multi-agent workflow graph under a name and start it again later (on demand or on a schedule) without retyping the definition. Use when the user wants to keep, re-run, or repeat a graph by name; not for designing a brand-new one-off graph.
---

# dag-library

A small personal/team library of `workflow` definitions. Each entry is one JSON file; running it means "read the file, give it a fresh key, fill placeholders, call `workflow` start".

This skill only stores and replays graphs. To design a new graph from scratch, compose it with the harness's `workflow` tool (or whatever graph-authoring skill your build ships). Come back here once the graph exists and the user wants to keep it.

## When to use

- "Save / keep this graph", "store what we just ran".
- "Run the `<name>` graph again", "re-run last night's audit".
- "Do this every night / every release" — the same pipeline on a cadence.

Skip it for a graph that will run once and never again.

## The library

A stored definition is a file `<name>.json` whose content is exactly a `workflow` `definition` object: `{ "key", "name", "nodes" }`.

Names: letters, digits, `.`, `-`, `_` only. Anything containing `/`, `\` or `..` is refused.

Lookup order (first hit wins):

1. Every directory in `MHC_DAG_LIBRARY`, split on the platform path delimiter (`:` on Unix, `;` on Windows so `C:\...` survives).
2. `<cwd>/.mhc/dags/` — project library, can be committed and shared.
3. `<home>/.mhc/dags/` — user library, follows the user across projects.

If nothing matches, say so and list every directory that was searched (the helper's error already does).

## Helper

`scripts/dag-lib.mjs` (JS) and `scripts/dag_lib.py` (Python) live in this skill's directory and implement the rules below. Below, `SKILL_DIR` means the absolute path of the folder holding this file.

| Function | What it does |
|---|---|
| `load(name, { suffix })` | find, validate, rotate key, fill placeholders; returns `{ definition, path, baseKey, key }`; **does not start** |
| `start(name, { suffix })` | `load` + `tool.workflow({ action: "start", definition })`; returns `{ runId, reused, warnings, key, path }` |
| `waitFor(runId)` | `tool.workflow({ action: "wait", run_id, detach: false })` — blocks until `completed` / `failed` / `cancelled` |
| `save(name, definition, { scope, force })` | validate and write `<scope root>/.mhc/dags/<name>.json`; refuses to overwrite without `force` |
| `searchDirs()` / `locate(name)` | show the search path / where a name resolves |

Shell use: `node $SKILL_DIR/scripts/dag-lib.mjs where | load <name> [--suffix S|--no-suffix] | save <name> --scope user|project [--force] < def.json`.

## Key rotation — read this before every run

`workflow` start is idempotent: the same `key` with the same graph in one session hands back the existing run (`reused: true`) instead of doing the work again. So the stored `key` is only a **base**:

| You pass | Final key | Effect |
|---|---|---|
| nothing (default) | `<base>-<UTC YYYYMMDD-HHmmss>` | always a new run |
| `suffix: "2026-10-26"` or `"v1.4.0"` | `<base>-<suffix>` | same suffix → same run (safe to re-issue after a crash); new suffix → new run |
| `suffix: ""` | `<base>` | deliberate reuse of the previous result; only when the user asks for it |

A node failed inside a suffixed run? Do **not** invent a new suffix. Call `workflow` with `action: "retry"` (and `node_ids`, or `node_id` + a new `prompt`) on that same `run_id`, or `action: "amend"` with an edited definition. Completed nodes keep their results.

## Placeholders

Any string in the definition may contain:

- `{{key}}` → the rotated key of this run
- `{{date}}` → UTC `YYYYMMDD`
- `{{datetime}}` → UTC `YYYYMMDD-HHmmss`

Substitution happens after rotation, so `{{key}}` is the final key.

Rules for authors:

- Put `{{key}}` in every output path a node writes (`reports/{{key}}/findings.md`). Otherwise the next run overwrites the last one.
- `dependsOn` orders nodes; it does **not** move data. Nodes hand results to each other through files, and each prompt must make sense to a reader who has seen nothing else.
- Prompts must be English, and should carry the markers `TASK:`, `DELIVERABLE:`, `SCOPE:`, `VERIFY:`, `STOP WHEN`. Graphs with two or more nodes should include a node whose id or prompt is about verifying/auditing. Without these, `start` returns lint warnings.

A complete example lives at `references/example-nightly-check.json`.

## Running

### JS code-mode cell

All kernel helpers are async — await every one.

```js
const lib = await import(`${SKILL_DIR}/scripts/dag-lib.mjs`);
const run = await lib.start("nightly-check");              // or { suffix: "20261026" }
if (run.warnings.length) print("lint warnings:", run.warnings);
const settled = await lib.waitFor(run.runId);               // omit to let the session be woken instead
print({ key: run.key, run_id: run.runId, reused: run.reused, status: settled?.status ?? settled });
```

Without the helper (same steps by hand):

```js
const base = JSON.parse(await read(".mhc/dags/nightly-check.json"));
const t = new Date().toISOString().replace(/[-:]/g, "").replace("T", "-").slice(0, 15); // YYYYMMDD-HHmmss UTC
const key = `${base.key}-${t}`;
const vars = { key, date: t.slice(0, 8), datetime: t };
const definition = JSON.parse(JSON.stringify({ ...base, key }).replace(/\{\{(key|date|datetime)\}\}/g, (_, k) => vars[k]));
const started = await tool.workflow({ action: "start", definition });
const settled = await tool.workflow({ action: "wait", run_id: started.run_id, detach: false });
```

(The string-replace shortcut is fine because the three values contain no characters that need JSON escaping.)

### Python code-mode cell

```python
import importlib.util, os
spec = importlib.util.spec_from_file_location("dag_lib", os.path.join(SKILL_DIR, "scripts", "dag_lib.py"))
dag_lib = importlib.util.module_from_spec(spec); spec.loader.exec_module(dag_lib)

loaded = dag_lib.load("nightly-check")                 # suffix="20261026" or suffix="" as needed
started = tool.workflow({"action": "start", "definition": loaded["definition"]})
settled = tool.workflow({"action": "wait", "run_id": started["run_id"], "detach": False})
```

Pass `"detach": False` explicitly: a bare `wait` on a live run detaches and returns at once. If the kernel's `tool.*` helpers are coroutines, prefix each call with `await` (or use `await dag_lib.start_async(name, tool.workflow)`).

### Outside code mode

Read the file with the normal read tool, rotate the key and fill placeholders yourself following the table above, then call `workflow` with `action: "start"` and the filled definition. Use `action: "wait"` with `detach: false` if you need to block.

### Load only

To inspect a definition or feed it to `amend`, call `load` (or `node dag-lib.mjs load <name>`) and stop there; nothing runs.

### Inspecting runs

`/dag` lists the session's graph runs; `/dag <run>` shows one run's wave tree.

## Saving

1. Settle the scope. User library (`~/.mhc/dags`) if it is the user's own habit; project library (`<repo>/.mhc/dags`) if the team should share or commit it. Ask if the request does not make this clear.
2. Pick and validate a name.
3. Make sure the definition follows the author rules (`{{key}}` in output paths, prompt markers, a verify node).
4. If a file of that name exists, tell the user and get a yes before overwriting (`force`).
5. Write it (`save(...)` creates the directory).
6. Verify by starting it once through the library path with default rotation and report the `run_id` — unless the user said not to. A run costs real agent work: if the graph is large or slow, confirm before this step.

## Scheduling

For "run it every X", hand the cadence to whatever scheduler the harness offers. The scheduled action is simply: load `<name>` by name with default rotation (or a `{{date}}`-style suffix such as today's UTC date, so a retried trigger on the same day reuses instead of duplicating) and start it. This skill does not schedule anything itself.

## What to report

After a run: definition name, final key, `run_id`, `reused`, terminal status, and where outputs went (the `{{key}}` paths). Show any lint `warnings` from `start`; never drop them silently.

After a save: absolute file path, scope, and the verification run's `run_id` and status (or that the user skipped it).

## Hard rules

- No un-rotated key unless the user explicitly wants the previous run back.
- No path-shaped names; nothing written outside the library directories.
- Every async kernel helper is awaited.
- Node prompts are English.
