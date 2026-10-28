# Format after edit — behavior spec (Phase 5)

Scope: after a file-mutating tool call succeeds, run the project's formatter on each file the
call changed and tell the model when a file's content changed. The MIT side already has the
place for this: `maho_lsp::hooks::HookChain` runs formatters before the post-mutation hooks
(LSP diagnostics, ...) and appends their texts to the tool result. This spec describes the
formatter that is plugged into that chain and the user-facing `formatOnMutation` keys
(`config-keys.md`). Proposed crate: `crates/maho-format` (any neutral name is fine).

## 1. Trigger

- Runs on the result of the tools `write`, `edit` and `apply_patch` (interface fact: these
  tool names). Any other tool name is ignored.
- Runs once per tool result, after the tool wrote its files and before diagnostics hooks, so
  diagnostics see the formatted text.

## 2. Which files

Paths are collected from the tool call, in this order, dropping empty strings and exact
duplicates (first occurrence wins; no trimming, so names with spaces are kept as written):

1. input keys `path`, `filePath` (strings);
2. input keys `paths`, `filePaths` (arrays; non-string items are skipped);
3. files named by the patch itself — the patch text in input key `input`, plus any
   per-file list the `apply_patch` tool reports in its result details. In the patch text the
   header lines `*** Add File: `, `*** Update File: ` and `*** Move to: ` (interface fact: the
   patch grammar) name files; the rest of such a line is trimmed;
4. objects inside input arrays `files` and `changes`: their `path`, `filePath`, `movePath`.

Each path is resolved against the session cwd (absolute paths stay absolute) and normalized
lexically (`.` removed, `..` pops a component; no symlink resolution).

A file is skipped when any of these hold (checked in this order):

- it does not exist (for example the old side of a move, or a deletion);
- its size exceeds `formatOnMutation.maxFileBytes`;
- no formatter applies to it (§3);
- formatting is off for its language (§4);
- git reports it as ignored (`git check-ignore` relative to the cwd; any git failure, or no
  repository, means "not ignored").

## 3. Formatter choice

Detection looks only at the **names of entries directly in the session cwd** (no walk up, no
recursion), computed at most once per tool result. First match wins (marker names, extensions and
command lines are interface facts: conventions of the external formatters):

| Marker in cwd | File extension | Formatter | Command line |
| --- | --- | --- | --- |
| `biome.json` or `biome.jsonc` | any | biome | `biome format --write <file>` |
| `.prettierrc*` or `prettier.config.*` | any | prettier | `prettier --write <file>` |
| `rustfmt.toml` or `Cargo.toml` | `.rs` | rustfmt | `rustfmt <file>` |
| `go.mod` | `.go` | gofmt | `gofmt -w <file>` |
| `ruff.toml`, or `pyproject.toml` with a line starting with `[tool.ruff]` (leading spaces allowed) | `.py` | ruff | `ruff format <file>` |

- Biome wins over prettier when both markers are present; the biome and prettier rows apply to
  every extension (the tool itself decides what it can format).
- Prettier → Bun substitution: when the prettier row matched and the project is a Bun project,
  use `bun format --write <file>` instead (reported tool name `bun format`). A Bun project is
  found by walking up from the cwd: stop with "no" at the user's home directory or at a
  directory containing `.git`; stop with "yes" at a directory containing `bun.lock`,
  `bun.lockb` or `bunfig.toml`, or whose `package.json` has a `packageManager` starting with
  `bun@`. The substitution is refused when any of the following exist: the nearest
  `package.json` defines `scripts.format`; a
  `node_modules/.bin/format` executable exists in the cwd or any parent; the cwd contains a
  `format` file, a `format.<js/ts-family extension>` file, or a `format/` directory with a
  `package.json` or `index.<js/ts-family extension>`. Finally `bun` must resolve to a binary
  and prove it formats: in a fresh temp directory, `bun format --help` succeeds and
  `bun format --write` actually rewrites a small unformatted TypeScript file (both within
  10 s). Any failure keeps prettier.
- Binary lookup: project-local tool directories first (for example `node_modules/.bin` up
  from the cwd), then `PATH`. Use the same resolver the LSP server installer uses.

## 4. Configuration

Keys under `formatOnMutation` (top level, `[native]`/`[codex]` blocks, profiles — see
`config-keys.md`), read from the merged config of the session cwd:

- `mode`: `off` | `best-effort` (default) | `required`. `off` disables everything, nothing is
  read or spawned.
- `languages`: map of language name → boolean. `false` turns formatting off for that language;
  `true` or absent means "use `mode`". Language name: `.ts`/`.tsx` → `typescript`, `.py` →
  `python`, otherwise the bare extension (`rs`, `go`, `js`, ...).
- `maxFileBytes` (default 1048576) and `timeoutMs` (default 3000); snake_case spellings
  `max_file_bytes` / `timeout_ms` are accepted on read.
- An invalid `formatOnMutation` value is treated as absent (defaults apply).

## 5. Running a formatter

1. Optional LSP path: when a language server is available for the file, ask it to format the
   file first. A "formatted" answer with added/removed line counts is the result. An answer
   that the server lacks formatting support, or that formatting is unavailable, falls through
   to step 2. Any other answer (including a successful reply with no detail) means
   "unchanged" — no CLI run.
2. CLI path: resolve the binary (§3). Not found, not spawnable, or a wait error → **missing**.
3. Read the file, run the command with cwd = session cwd, stdin/stdout/stderr discarded, no
   console window on Windows, killed after `timeoutMs`.
4. Non-zero exit or timeout → restore the original bytes, result "unchanged".
5. Otherwise compare before/after: equal → "unchanged"; different → "formatted" with
   line counts: count = number of `\n`-separated segments (so a trailing newline adds one);
   added = max(0, after − before), removed = max(0, before − after).

Concurrent format requests for the same absolute path (two tool results racing) share one run:
later callers wait for and reuse the first caller's result; a cancelled run does not block the
next caller.

## 6. Output

- Per formatted file, one text block appended to the tool result, worded by the implementer.
  It must carry: the file path relative to the cwd with `/` separators, the formatter's name,
  the added and removed line counts written as `+<added>/-<removed>`, and the fact that the file
  on disk now differs from what the tool wrote. Tests match on the path, the name and the
  `+a/-r` token only.
- Missing formatter in `best-effort`: no change to the tool result; log one warning per
  (session, formatter name) pair naming the formatter.
- Missing formatter in `required`: the tool result is marked as an error and gets a text
  naming the formatter and the path as the tool reported it. Other files are still processed.
- "unchanged" and skipped files produce nothing.

## 7. Platform notes

- Windows: `.cmd`/`.bat` shims are launched through `cmd.exe`; Bun shim suffixes `.exe`,
  `.bunx`, `.cmd` are checked when looking for a shadowing `format` executable.
- Paths in notices always use `/`.

## 8. Acceptance tests

1. A `read` or `bash` tool result is ignored (no process spawned, result unchanged).
2. `mode: "off"` with a `biome.json` marker: no formatter runs, result unchanged.
3. Path collection from `{path, filePath, paths, filePaths}` keeps a name with surrounding
   spaces as written, drops `""` and a non-string item, and de-duplicates.
4. An `apply_patch` input with `*** Update File:  lib/x.py ` and `*** Add File: y.md` plus
   `changes: [{path: "z.go", movePath: "w/z.go"}]` yields `lib/x.py, y.md, z.go, w/z.go`.
5. `docs/../docs/r.md` and `./pkg/./m.go` normalize to `<cwd>/docs/r.md` and `<cwd>/pkg/m.go`.
6. Markers `biome.jsonc` + `prettier.config.mjs` → biome; `.prettierrc.yaml` alone → prettier;
   `rustfmt.toml` with `m.rs` → rustfmt; `go.mod` with `m.go` → gofmt; `pyproject.toml`
   containing `  [tool.ruff]` with `m.py` → ruff; the same with only `[tool.isort]` → none.
7. `Cargo.toml` with a `.go` file → none; no markers at all → nothing formatted.
8. A file larger than `maxFileBytes` and a git-ignored file are skipped.
9. `languages: {"python": false}` with `mode: "required"`: `.py` is skipped, `.ts` uses
   `required`.
10. Config parses `{"mode":"off","languages":{...},"maxFileBytes":2048,"timeoutMs":750}`
    and the snake_case spellings; defaults are best-effort / 1048576 / 3000.
11. Fake formatter script that rewrites the file adding two lines: one appended block with the
    relative path, the formatter name and `+2/-0`.
12. Fake formatter that exits non-zero after rewriting: file bytes equal the original, no block.
13. Fake formatter that sleeps past `timeoutMs`: it is killed, file restored, no block.
14. Fake formatter that leaves the file as is: no block.
15. Line counts: `x\ny\n` → `x\n` reports `+0/-1`; `p` → `p\nq\nr` reports `+2/-0`.
16. Binary missing, `best-effort`: no error, exactly one warning for two calls in the same
    session, a second warning for another session.
17. Binary missing, `required`: result marked error, text names the formatter and the path.
18. LSP path reports "formatted" with counts 4/3: block shows `+4/-3`, no CLI process runs.
    LSP path reports "unavailable": CLI runs. LSP success without detail: no CLI, no block.
19. Two concurrent requests for one path run the formatter once and both get its result; after
    the first run is cancelled, a new request on that path runs normally.
20. Bun substitution: a temp project with `bunfig.toml` and a fake `bun` that passes both probes
    → `bun format`; adding `scripts.format` to `package.json`, or a `format/index.js` next to
    a `format/package.json`, or a fake `bun` whose `--write` does not change the probe file →
    prettier.
21. Bun detection stops at a `.git` directory or the home directory without a Bun marker.
