# Spec: helper utilities still needed by MIT code (Phase 1)

Status: draft for review · Audience: clean-room implementer · Proposed home: a small
`maho-support` crate (or fold each helper into the crate that uses it; prefer folding when a
helper has a single consumer).

The SUL `utils` crate is a grab bag (~13k lines of source). Most of it is either (a) replaceable
by a well-known crate, (b) already present in MIT form inside mhc (`maho-core`, `maho-ai`,
`maho-agent`, `maho-cli::utils`), or (c) specific to features that are removed or rewritten in
later phases. This spec lists, for every helper that a Phase 1–3 MIT consumer will need, the
**contract** (behaviour, inputs, outputs, errors) and whether to use a standard crate.

Legend for "Plan": **STD** use the named crate · **MIT** reuse existing mhc MIT code · **NEW**
write from this contract · **DROP** do not port (reason given).

---

## 1. Inventory and decisions

| Area | Plan | Consumers (future MIT crates) |
| --- | --- | --- |
| JSONC parse / read file | STD `jsonc-parser` (+ `serde_json`) | maho-config, model profile (reads `models.json` policy), skill loader |
| Atomic file write | NEW (small) or STD `tempfile::NamedTempFile::persist` + fsync | maho-config writer/journal, memory, task state |
| YAML front-matter | NEW thin wrapper over STD `serde_yaml`/`serde_yml` | skill loader, command/agent loaders (Claude-compatible) |
| Markdown-file and symlink helpers | NEW (trivial) | skill/command loaders |
| Path containment | NEW (small); STD `path-clean`/`dunce` may help | task isolation, skill path expansion, tools |
| Data directory | STD `dirs`/`directories` + NEW fallback rule | memory store, caches |
| `${VAR}` expansion with allow-list | NEW | MCP config, hooks, gateway |
| Skill `@path` reference expansion | NEW | skill loader |
| PATH lookup (`which`) | STD `which` crate (with the safety rule in §2.9) | LSP, ast-grep, git bash, formatter |
| Process run with tree timeout | MIT check first (`maho-cli::utils::child_process`, `maho-core` shell); else NEW | tmux, formatter, comment checker, LSP probes |
| Spawn + abort | MIT `maho-ai::utils::abort` + `tokio::process` | all |
| Shared diagnostic logger hook | NEW (tiny) or MIT `maho_ext_api::diagnostic` | maho-config, model selection, task |
| Rotating product log file | MIT check first; else NEW per §2.13 | task/memory debug logs |
| Deep merge of JSON objects | NEW (lives in maho-config, config.md §5) | maho-config |
| Unique-string merge | NEW (one-liners) | skill/agent lists |
| camelCase ↔ snake_case key transform | STD `heck` + NEW walker | RPC/config bridges |
| Duration / semver formatting | STD `humantime` / `semver` (+ regex) | doctor, LSP install |
| Free port finder | STD `std::net::TcpListener` bind probe (+ NEW scan) | gateway/server |
| Git-bash resolver (Windows) | NEW only if `git_bash` tool is kept (plan §8 Q6) | git-bash MCP |
| ast-grep binary resolver | DROP if ast-grep moves to `maho-ext-pi-ast-grep` (plan §2) | — |
| Shell escaping for double quotes | NEW (tiny) or STD `shell-escape` | hooks, tmux |
| Legacy agent/hook/model renames (`migration::*`) | DROP — they serve the OpenCode import (config.md Q3) and rename OmO persona agents | — |
| Prompt async gate / session idle settle / internal initiator marker | DROP here — part of the task runtime; respecified with Phase 2 task (oh-my-pi base) | — |
| Code-graph provisioning/sweepers | DROP — the setting is retired (config.md §12.1 removes `codegraph`) | — |
| LSP daemon process sweep | DROP here — respecify with Phase 2 LSP | — |
| Archive/zip entry validation | DROP here — only used by installers; reuse STD `zip` + path containment if needed | — |
| Tool-name transform, record type guards, misc JS shims (`bun_file`, `bun_write`) | DROP — JS-runtime shims; use `std::fs` / `serde_json::Value::is_object` | — |
| Plugin config detection (`detect_config_file`, OpenCode plugin names) | DROP — OpenCode-specific | — |
| Old `omo.json` validator in utils | DROP — superseded by maho-config | — |
| Product identity / OSC strings / feedback-text normaliser | DROP here — respecify with the feature that needs them | — |

---

## 2. Contracts for helpers that are kept

### 2.1 JSONC
- `parse_jsonc_lenient(text) -> { value: Option<Value>, errors: Vec<{message, offset}> }`:
  accepts `//` and `/* */` comments and trailing commas; returns **all** syntax errors found with
  byte offsets; `value` is absent for empty input.
- `parse_jsonc(text) -> Result<Value, SyntaxError>`: first error only.
- `read_jsonc_file(path) -> Option<Value>`: `None` when the file is missing, unreadable or has any
  syntax error (never panics).
- Plan: wrap `jsonc-parser` (it supports comments and trailing commas and returns positions).

Tests: comments + trailing commas parse; a document with two missing values reports two errors,
each with its byte offset; missing file → `None`.

### 2.2 Atomic write
`write_atomically(path, bytes)`:
1. create a temp file next to the target with a name unique per process and call (e.g.
   `<path>.<pid>.<counter>.tmp`), opened create-new;
2. write all bytes, fsync the file;
3. rename over the target; on Windows, if the rename fails with permission denied, delete the
   target and rename again;
4. on non-Windows, fsync the parent directory;
5. fsync failures with "permission denied / unsupported / invalid argument" kinds are tolerated
   (some filesystems cannot fsync); other failures abort;
6. on any failure, remove the temp file and return the error; the target is either the old or
   the new content, never partial.

Tests: target replaced; temp removed after an injected write failure; injected `EINVAL` fsync
error is tolerated; injected `EIO` fsync error aborts and leaves the target unchanged.

### 2.3 Front-matter
`parse_front_matter(text, mode) -> { data: JSON object, body: String, had_front_matter: bool,
parse_error: bool }`:
- strip a leading UTF-8 BOM;
- default mode: front matter is a block that starts at offset 0 with `---` + newline (LF or
  CRLF) and ends at the next line that is exactly `---` followed by a newline; the content between
  is YAML; empty YAML → `{}`; YAML `null` → `{}`; YAML that fails to parse → `data = {}`,
  `parse_error = true`, body still returned;
- no front matter → `data = {}`, body = whole text, `had_front_matter = false`;
- "rule" mode: tolerant variant for rule files — accepts an opening `---` line and a closing `---`
  line even when the closing line is the last line without a trailing newline; same output
  shape. (Respecify details with the rules feature if needed.)

### 2.4 File helpers
- `is_markdown_file(name, is_file) -> bool`: true when it is a regular file whose name ends in
  `.md` (case-sensitive; `.MD` is not markdown) and does not start with `.`.
- `resolve_symlink(path) -> PathBuf`: canonical path if resolvable, else the input unchanged.
- `is_symlink(path) -> bool`: `symlink_metadata` says symlink; errors → false.

### 2.5 Path containment
`contains_path(root, candidate) -> bool`: both paths are made absolute (relative to cwd),
lexically normalised (`.`/`..` removed), then canonicalised; for a path that does not exist yet,
canonicalise its deepest existing ancestor and re-append the remainder. True when the candidate
equals the root or is below it (component-wise, not string prefix: `/a/bc` is not inside `/a/b`).
`is_within(candidate, root)` is the same with swapped argument order.

Tests: symlinked root accepted when the candidate is under its real path; non-existent file
under root → true; `..` escape → false; sibling with shared prefix → false.

### 2.6 Data directory
`data_dir(app_name, env, os) -> PathBuf`: `$XDG_DATA_HOME` if set, else `<home>/.local/share`;
use it if it can be created and is a writable directory; otherwise fall back to
`<tmp>/<app_name>-data` (created best-effort). `env` and `os` (home, tmp) are injectable.

### 2.7 Environment reference expansion
`expand_env(text, opts) -> String` replaces `${NAME}` and `${NAME:-default}`:
- value from `opts.env` (default: process env);
- unset → the default text if given, else empty string (a variable set to the empty string
  expands to empty, not to the default);
- when `opts.trusted` is false and an allow-list predicate is supplied, a name the predicate
  rejects is **not** read from the environment: it is replaced by its default text if given,
  else by empty string, and `opts.on_blocked(name, NotAllowed)` is called once per occurrence;
  with no predicate every name is allowed;
- `expand_env_in_json(value, opts)`: applies to every string value recursively (object keys are
  not expanded), dropping unsafe keys (`__proto__`, `constructor`, `prototype`).


### 2.8 Skill `@path` references
`expand_skill_refs(content, skill_dir) -> String`: an `@` that is not preceded by a letter,
digit, `=`, `"` or `(` and is followed by `<segment>/<rest>` where the last segment has a file
extension is replaced by the absolute path `skill_dir/<segment>/<rest>`, **only** if that path
stays inside `skill_dir` after normalisation (both POSIX and Windows separators handled);
otherwise the text is left unchanged. E-mail-like and decorator-like `@` uses are not touched.

### 2.9 PATH lookup
`which_safe(name) -> Option<PathBuf>`: reject empty names and names containing `/`, `\`, `..`,
NUL, equal to `.`, or starting with a drive prefix (`C:`); search `PATH` (or `Path`) entries in
order, skipping empty entries; on Windows try `.exe`, `.cmd`, `.bat`, `.com` when the name has
no extension; a candidate must be a regular file (and executable on Unix). Plan: call the
`which` crate after applying the rejection rule.

### 2.10 Process with tree timeout
`run_with_tree_timeout(cmd, args, cwd, env, timeout, max_output_bytes) -> { exit status, stdout,
stderr (each capped), timed_out, termination report }`: start the child in its own process group
(Unix) / job (Windows); collect output up to the byte cap (excess discarded, flag set); on
timeout send a graceful signal to the whole tree, wait a short grace period, then force-kill
survivors; the report lists each signal attempt and outcome. Never leaves orphaned descendants.
Check `maho-cli::utils::child_process` and `maho-core::utils::shell` first; extend them instead
of adding a second implementation.

### 2.11 Logger hook
A process-wide optional sink `fn(message, data: Option<&Value>)`: `set_sink(Option<Sink>)`,
`log(message, data)` is a no-op until a sink is set. Consumers in maho-config and model
selection log through it so the host decides where lines go (mhc: `maho_ext_api::diagnostic`
or a component logger).

### 2.12 Small helpers
- `merge_unique(a, b)` / `merge_unique_ci(a, b)`: concatenation preserving first occurrence order,
  exact or case-insensitive dedupe.
- `camel_to_snake`, `snake_to_camel`, `transform_keys(value, shallow|deep)`: use `heck`; map
  object keys only, never string values.
- `format_duration(ms)`: negative treated as 0; whole seconds; `Hh Mm Ss` when hours > 0,
  `Mm Ss` when minutes > 0, else `Ss` (lower units are shown even when zero, e.g. `1h 0m 5s`).
- `extract_semver(output)`: first `MAJOR.MINOR.PATCH` (with optional `-pre`/`+build` tails,
  optional leading `v` which is not returned) in a tool's `--version` output, ignoring a match
  directly preceded by a digit or `:` (so `00:24:25.202` timestamps are not versions); `None` for
  blank output.
- `find_free_port(start, host)`: first port ≥ start that binds on `host` (default
  `127.0.0.1`), error after the 65535 ceiling.
- `escape_for_double_quotes(s)`: prefix each of `\` `$` `` ` `` `"` `;` `|` `&` `#` `(` `)` with a
  backslash; other characters unchanged.

### 2.13 Rotating log file (only if no MIT equivalent exists)
`Logger::new(file_name, {max_size=50 MiB, max_backups=2, flush_interval=500 ms,
buffer_limit=50 lines, path_resolver})`: lines are JSON or text with a timestamp; buffered and
flushed on interval or when the buffer limit is hit; when the file exceeds `max_size`, rotate
`file → file.1 → file.2`, dropping the oldest beyond `max_backups`. Default directory: OS temp
dir unless a resolver is given. mhc already writes `logs/fallback.log` with size capping in
`maho-core::retry_fallback::log`; prefer extending that.

---

## 3. API surface the implementer must provide

Only what Phase 1 crates call directly:

| Function | Phase 1 caller |
| --- | --- |
| `parse_jsonc_lenient`, `read_jsonc_file` | maho-config loader/writer/migrator; model profile's credential-rotation read of `<agent dir>/models.json` |
| `write_atomically` (+ "write exclusive" create-new helper) | maho-config writer, journal, lock |
| `contains_path` | maho-config (backup-path safety), later task isolation |
| logger hook | maho-config, model selection |
| `which_safe` | later phases |

Everything else in §2 is needed by Phase 2/3 consumers and can be written when they land.

---

## 4. Open questions

- **Q1** Does any MIT crate already provide tree-timeout process runs equivalent to §2.10? If so,
  drop the NEW item.
- **Q2** Keep Windows `git_bash` support at all (plan §8 Q6)? If dropped, the resolver goes too.

---

## Leak self-check (spec writer)

- Contracts are written from observed behaviour; no code, comments or regex sources are copied.
  Patterns are described in words (e.g. "`@` not preceded by …") rather than reproduced.
- Constants that are behavioural defaults (50 MiB, 2 backups, 500 ms, 50 lines; Windows
  extension list) are stated as facts, not as copied declarations.
- No identifier is reused from the SUL crate's public API layout; names are proposals.
