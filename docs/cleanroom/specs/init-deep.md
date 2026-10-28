# Project context files (Phase 3 step 5c: init-deep and its advisor)

Status: behaviour spec for the MIT profile. Written from MIT sources and public behaviour only:

- oh-my-pi @ f59bbac3d (MIT) `packages/coding-agent/src/prompts/agents/init.md`: the `/init` idea —
  survey the codebase (sources, tests, build configuration, scripts and docs) and write one
  `AGENTS.md` at the project root covering overview, architecture, key directories, development
  commands, conventions, important files, tooling and testing, concise and practical.
- senpi @ 26acd9353 (MIT) `packages/coding-agent/src/core/extensions/builtin/nested-agents-md`:
  `AGENTS.md` files in subdirectories are read when the agent works below them (nearest first up to
  the project root), so per-directory files are useful and should not repeat their parents.
- Public behaviour of Claude Code `/init` and Codex `/init` (generate a project instructions file).
- Hierarchical generation (one file per significant directory, deepest first) is the extension
  named in the plan's precedent table; the rules below are new.

No SUL path, `crates/omo/**`, `assets/omo*`, `.omo/`, the omo `init-deep` skill text or any omo-gated
maho-cli module was read. Prompts, thresholds and names are new.

## 1. Analysis

Root: the git top-level of the working directory, else the working directory. Walk it honouring
`.gitignore`, `.ignore` and git excludes, skipping hidden entries and the fixed names `node_modules`,
`target`, `dist`, `build`, `out`, `vendor`, `__pycache__`, `.venv`, `venv`.

Per directory:
- `files`: regular files directly in it; `code_files`: those whose extension maps to a language
  (table in code: rs, ts, tsx, js, jsx, mjs, py, go, java, kt, swift, c, h, cc, cpp, hpp, cs, rb,
  php, scala, lua, sh, zig, ex, exs, dart, vue, svelte);
- `subtree_code_files`: code files in it and below;
- `languages`: language → count over the subtree, top 3 reported;
- `manifests`: build/package files directly in it (Cargo.toml, package.json, pyproject.toml,
  setup.py, go.mod, pom.xml, build.gradle, build.gradle.kts, CMakeLists.txt, Makefile, Gemfile,
  composer.json, deno.json, mix.exs, pubspec.yaml, *.csproj);
- `entry_points`: files named main.*, lib.rs, mod.rs, index.*, app.*, __init__.py, cli.* directly in it;
- `has_tests`: a child dir named `test`, `tests`, `__tests__`, `spec` or files matching `*_test.*`,
  `*.test.*`, `*.spec.*`, `test_*.py`;
- `context_file`: an existing `AGENTS.md` (or `CLAUDE.md` when there is no `AGENTS.md`).

## 2. Choosing directories

Settings (§6): `max_depth` (3), `max_dirs` (12), `min_files` (20).
- The root is always a target.
- A directory below the root is a candidate when its depth ≤ `max_depth`, and it has a manifest,
  or its `subtree_code_files` ≥ `min_files`.
- Directories with no files of their own (pass-through or grouping directories such as
  `crates/` or `web/`) are not candidates; their children are.
- A candidate whose subtree code files equal those of its only candidate child is dropped in
  favour of the child.
- A candidate without a manifest whose parent is a candidate with a manifest and the same
  `subtree_code_files` (a package's only code directory, such as its `src/`) is dropped in favour
  of the parent.
- Score = `subtree_code_files` + 40 × manifests + 10 × `has_tests`; the best `max_dirs - 1`
  candidates are kept (ties: shallower first, then path order).
- Targets are processed deepest first, so a parent can summarise its children.

## 3. Plan

`plan` (JSON, stable keys): `{ "root", "targets": [{ "path" (relative, "." for root), "depth",
"action", "files", "subtree_code_files", "languages", "manifests", "entry_points", "has_tests",
"children" (relative paths of targets directly below it) }] }`.
`action`: `create` (no context file), `update` (context file exists and update was requested),
`keep` (context file exists, no update requested).

## 4. Prompts

For each target a prompt is built from its facts: the directory, its role in the tree (root or
subdirectory of `<parent>`), facts from §1, the summaries of child targets already written (first
heading line and first paragraph, at most 600 characters each), and — for `update` — the current
file. The prompt asks for Markdown only, no code fence around the whole answer, a `# ` title naming
the directory, sections only where there is something specific (purpose, layout, commands,
conventions, gotchas, where to look first), no repetition of what a parent covers, at most about
120 lines for the root and 60 for subdirectories. The model may read files with read-only tools.

## 5. Running

`mhc init-deep [--root DIR] [--plan] [--update] [--dry-run] [--model M] [--max-depth N]
[--max-dirs N] [--min-files N] [--json]`:
- `--plan`: print the plan (JSON with `--json`, else a table); never calls a model.
- otherwise, for each target with action `create`/`update`, run a child `mhc -p --no-session
  --tools read,grep,find,ls [--model M]` with the prompt, in the target directory, timeout 600 s;
  its stdout, trimmed, with one wrapping code fence removed, is written atomically to
  `<target>/AGENTS.md` (`--dry-run`: printed instead). Empty output or a failed child → that target
  is reported failed and others continue. Exit 1 when any target failed.
- final line: `context files: N written, M kept, K failed`.

`/init-deep [update]` in a session: the analysis runs in-process and a user message is sent with
the plan and the same writing rules, asking the agent to write the files with its own tools,
deepest first.

## 6. Advisor and keys

At session start in long-lived modes (TUI, RPC, app server), inside a git repository only, once per
project root ever (recorded in
`<home>/.omo/state/context-advice.json` as a list of roots), when the root has no context file and
its analysis (bounded to 50 000 walked entries) finds at least `min_files` source files, the user gets an
info notification suggesting `/init-deep` (with the number of directories it would cover). Off with
`init_deep.advise: false` or `MAHO_CONTEXT_ADVICE=0`.

`omo.jsonc` section `init_deep`: `advise` (bool, true), `max_depth` (int ≥ 0, 3), `max_dirs`
(int ≥ 1, 12), `min_files` (int ≥ 1, 20). Command-line flags win over keys.

## 7. Tests (written from this spec)

Fixture trees: ignore rules and skipped names; manifests/entries/tests detection; candidate rules
(depth, pass-through, single-child collapse, max_dirs); deepest-first order; actions; prompt contains
facts and child summaries and no fence; fence stripping; advisor once per root and opt-outs.
