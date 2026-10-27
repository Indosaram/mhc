# Spec: layered `.omo` configuration (Phase 1)

Status: draft for review · Audience: clean-room implementer · Proposed crate: `maho-config`
(library) plus two small extensions, `config-notices` (startup) and `config-hot-reload`
(watch registration). Names are suggestions; keep them neutral (see README rule 4).

This spec covers the **harness configuration file** `omo.jsonc` / `omo.json` only. The agent
directory (`~/.omo/agent/settings.json`, `models.json`, `auth.json`, …) is already handled by
MIT code in `maho-core` and is out of scope, except where a path is mentioned for reference.

---

## 0. Scope

In scope (general features):

- where configuration files live and which ones are read;
- the JSONC file format and the key set users write;
- per-file validation that never lets one bad value disable a whole file;
- merge precedence across user and project files;
- per-harness blocks and named overlay profiles;
- defaults, final validation of the merged view;
- model catalog alias resolution inside the view (contract shared with
  [model-selection.md](model-selection.md));
- the union rule for disabled skills;
- diagnostics and their human rendering;
- a comment-preserving writer;
- one-shot migrations of older config shapes, with a lock, journal and backups;
- startup notices; hot reload validation.

Out of scope (owned by later phases, but their **keys must be accepted** so existing files are
not stripped — see [config-keys.md](config-keys.md)): the behaviour behind `task`, `teams`,
`memory`, `computer`, `side_panel`, `formatOnMutation`, `gateway`, `git_master`, `telemetry`.

Excluded entirely: anything belonging to OmO-original features (none of them have keys in this
file).

---

## 1. Terms

- **Layer**: one parsed configuration file.
- **User layer**: the file in the user's home `.omo` directory.
- **Project layer**: a file in some ancestor directory's `.omo` directory.
- **Merged document**: all layers deep-merged in precedence order (§5).
- **View**: the merged document after harness blocks and the active profile are applied and the
  control keys are removed (§6). Consumers only ever see a view.
- **Harness**: which product is reading the file. mhc always reads as the harness `native`.
  `senpi` is an accepted legacy spelling of `native` (callers may pass either; both mean
  `native`).
- **Harness block**: a top-level key spelled with brackets, e.g. `[native]`, holding overrides
  that apply only to that harness.
- **Profile** (overlay profile): an entry of the top-level `profiles` map, activated by name
  (§6.2). Not to be confused with **model profile** (`model_profile` / `model_profiles`), which
  is a model-chain feature specified in model-selection.md.
- **Control keys**: `profiles` and every harness block key. They steer resolution and never
  appear in a view.
- **Diagnostic**: a structured note produced while loading (§10).

---

## 2. File locations

### 2.1 Home directory

The home directory used for every path in this spec is, in order: the `HOME` value of the
environment passed to the loader, else `USERPROFILE`, else the current working directory. The
value is made absolute and normalised to forward slashes on all platforms (drive letters keep
their form, e.g. `C:/Users/me`). `XDG_CONFIG_HOME`, `APPDATA` and `~/.config` are **never**
consulted for the harness configuration.

Separately, the **account home** is the operating-system account's home directory (on Unix the
password database entry for the current user; elsewhere the process `HOME`/`USERPROFILE`). It
differs from the home directory only when `HOME` was overridden (tests, sandboxes).

### 2.2 User file

Directory: `<home>/.omo`. File: `omo.jsonc` if it exists, otherwise `omo.json` if it exists,
otherwise the (non-existent) `omo.jsonc` path is reported as the user candidate. When both
exist, only `omo.jsonc` is read; `omo.json` is ignored without a diagnostic.

The user file is read even if it (or `.omo`) is a symlink.

### 2.3 Project files (ancestor walk)

Starting at the working directory and moving to each parent:

1. If the current directory is a **boundary**, stop without reading it. Boundaries are the home
   directory and the account home. A directory is a boundary when it equals one of them
   literally, or when its symlink-resolved real path equals one of their real paths. On Windows
   the comparison is case-insensitive.
2. Otherwise look for `<dir>/.omo/omo.jsonc`, then `<dir>/.omo/omo.json` (first existing wins,
   as for the user file).
3. Stop at the filesystem root, or after 256 directories, whichever comes first.

Symlink protection (project files only):
- if `<dir>/.omo` is a symlink, that directory contributes nothing;
- if the chosen file itself is a symlink, it is skipped and the `.json` alternative is **also**
  not considered unless it is a regular file (a symlinked `.jsonc` does not "fall through" to a
  symlinked `.json`).

Effect: a working directory inside home walks up to (not including) home; a working directory
outside home walks to the filesystem root. Home's own `.omo` is never read twice (it is the
user layer).

### 2.4 Order of layers

1. the user candidate (always first, even when the file does not exist);
2. project files ordered **farthest ancestor first, nearest last**.

Every candidate yields a **source record**: path, scope (`user` or `project`), whether the file
exists, whether it was loaded.

---

## 3. File format

- Encoding UTF-8. Content is JSON with comments (`//`, `/* */`) and trailing commas allowed
  (JSONC). Use a standard JSONC parser (e.g. `jsonc-parser`) configured for both.
- An empty file or whitespace-only file is a parse result with no value: treat as "root is not
  an object" (§4.4).
- The root must be an object.
- Keys named `__proto__`, `constructor` or `prototype` are **unsafe** at every depth. They are
  removed wherever they occur and reported like unknown keys. They are never merged.
- A top-level `$schema` string is allowed and ignored.

The full key set, with types, bounds and defaults, is in [config-keys.md](config-keys.md).
Sections relevant to Phase 1 consumers:

| Key | Shape | Phase 1 meaning |
| --- | --- | --- |
| `agents` | map name → agent definition | per-agent model and options (model fields described in model-selection.md §2) |
| `categories` | map name → category definition | per-category model chain and options |
| `models` | map alias → `{ model, reasoning? }` | model catalog aliases (§8) |
| `model_profiles` | map id → `{ display_name?, family?, tier?, models? }` | model profile definitions (model-selection.md §6) |
| `model_profile` | string | active model profile id, or a literal `provider/model` pin |
| `disabled_skills` | array of strings | skill denylist (§9) |
| `profiles` | map name → profile body | overlay profiles (§6.2) |
| `[native]`, `[senpi]`, `[codex]` | typed harness block | same keys as a profile body minus `profiles` and harness blocks, plus `gateway` |
| `[opencode]` | any object | accepted and preserved; never read by mhc |
| `_migrations` | array of strings | migration markers (§12); written by the migrator |
| `legacy_migrations` | object (any) | history copied from imported legacy files; preserved, not interpreted |

A **profile body** may contain every ordinary section except `gateway` and `$schema`, plus the
four harness blocks. A **typed harness block** may contain every ordinary section including
`gateway`, but not `profiles`, not `$schema`, not other harness blocks.

---

## 4. Reading one layer

Given a candidate path:

### 4.1 Missing file
Source record `exists=false, loaded=false`. No diagnostic.

### 4.2 Unreadable file
Source record `exists=true, loaded=false`; one **read** diagnostic naming the path and the OS
error.

### 4.3 Parse error
Any JSONC syntax error makes the file not loaded; one **parse** diagnostic listing every error
found (all errors, not only the first). The file contributes nothing.

### 4.4 Validation (lenient, smallest-subtree)

Validation uses the *layer* rules: every field optional, no defaults applied, strict about
unknown keys. Steps, in order:

1. Remove unsafe keys at every depth. If any were removed, emit one **unknown-keys**
   diagnostic listing their dotted paths.
2. If the root is not an object → not loaded, **validation** diagnostic ("root must be an
   object").
3. Validate. If valid → loaded.
4. Otherwise remove every key the rules do not know (at any depth, including inside harness
   blocks and profiles). Emit one **unknown-keys** diagnostic listing the removed dotted paths.
   Re-validate; if valid → loaded.
5. Otherwise **prune**: for each remaining issue, delete the smallest failing subtree — the
   value at the issue's path, or, if the path does not exist (a missing required field), the
   deepest existing ancestor object. Delete later array indices before earlier ones. Remove
   containers that became empty because all their children were deleted. Re-validate; repeat.
   Each deleted value yields one **invalid-value** diagnostic carrying the dotted key and the
   rule's message.
6. The file is **rejected** (not loaded, one **validation** diagnostic listing every issue path)
   when: an issue targets the root itself; more than 32 prune passes were needed; or nothing
   valid is left.

Healthy siblings survive at every depth: an invalid `agents.x.model` does not remove
`agents.x.description` or `agents.y`.

### 4.5 Legacy normalisation on read (preprocessing)

Applied to every *model-bearing object* (an agent definition, a category definition, an object
entry of a `models` list, a `fallback_models` object entry, a `model_profiles` entry, a `models`
catalog entry) **before** its rules are checked:

- `model` strings are canonicalised: a trailing reasoning token written as `name (high)` or
  `name high` becomes `name:high` when the token is a known reasoning level; `none` reads as
  `off`; `:max` is accepted only when the model part is provider-qualified (`a/b:max`). Unknown
  tokens leave the string unchanged.
- The legacy fields `variant`, `reasoningEffort`, `thinking`, `textVerbosity`, `providerOptions`
  are folded into the canonical ones: `reasoning` takes, in priority, explicit `reasoning`, then
  `reasoningEffort`, then `variant`, then `off` when `thinking.type` is `disabled`.
  `provider_options` starts from `provider_options` (else `providerOptions`); an enabled
  `thinking` object is copied to `provider_options.thinking`; `textVerbosity` is copied to
  `provider_options.textVerbosity`. `maxTokens` (number) becomes `max_tokens` when `max_tokens`
  is absent.
- Folding happens before the unknown-key check, and the folded legacy fields (`variant`,
  `reasoningEffort`, `thinking`, `textVerbosity`, `providerOptions`, numeric `maxTokens`) are
  removed from the value being checked. A config written in the old style is therefore never
  reported as having unknown keys, on any model-bearing object. The view contains only the
  canonical fields (`model`, `reasoning`, `provider_options`, `max_tokens`, …). config-keys.md
  lists the legacy fields under each section for schema generation.

Reasoning levels: `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, plus `auto`. A
`reasoning` value may also be any other string (passed through, lower-cased by consumers).

### 4.6 Legacy name canonicalisation on read

After a layer validates, and before it is merged:

- **Category rename**: a category named `deep` (as a key of any `categories` map at any depth,
  or as the string value of any `category` field at any depth) is renamed to `deep-low`. If the
  same `categories` map already has `deep-low`, the `deep` entry is dropped instead.
- **Harness rename**: a `[senpi]` block (top level or inside a profile) is renamed to `[native]`;
  if `[native]` already exists alongside it, `[senpi]` is dropped.
- Each rename set yields one **deprecated-keys** diagnostic for the file naming each path and
  whether it was renamed or ignored. The migrator (§12) rewrites the file itself later; reading
  keeps working even when that rewrite cannot happen (read-only project file, migration locked).

The raw, canonicalised layer (before merge, with control keys intact) is kept in the result as
`layers` together with its source record, so later consumers (§9, debugging) can inspect
provenance.

---

## 5. Merge precedence

Layers are merged left to right in the §2.4 order; a later layer wins.

Merge rule (deep merge):
- object + object → recursive merge, key by key;
- anything else → the later value replaces the earlier one (arrays are **replaced**, never
  concatenated; a later `null`… cannot occur because the rules reject nulls where objects are
  expected);
- unsafe keys are dropped from the incoming side at every depth.

Exception: `disabled_skills` is a union across layers when consumers ask for the skill denylist
(§9); in the merged document itself it follows the replace rule.

---

## 6. View resolution

### 6.1 Harness blocks

For harness `native`, the harness overlay is built from the merged document by merging
`[senpi]` first and then `[native]` on top (so `[native]` wins every key both define). Other
harness blocks are ignored for the view (but still validated, so bad keys inside `[codex]` are
reported).

### 6.2 Overlay profiles

Active profile name, first non-empty of:
1. an explicit name passed by the caller;
2. env `OMO_PROFILE`;
3. env `OCX_PROFILE`;
4. env `OPENCODE_CONFIG_DIR` when its normalised path ends with `/profiles/<name>` (exactly one
   segment after `profiles/`, trailing slashes ignored, backslashes treated as slashes), or is
   `profiles/<name>`.

If a name is active but `profiles.<name>` is missing or not an object: emit one **profile**
diagnostic (path `profiles.<name>`, saying the base configuration is used) and resolve as if no
profile were active. The result's `profile` field is the name only when it existed.

### 6.3 Composition

The view is the deep merge, in order, of:
1. the merged document without control keys;
2. the harness overlay of the merged document (§6.1), without control keys;
3. the profile body without control keys (if a profile is active);
4. the harness overlay *inside* the profile body (if a profile is active).

Control keys are stripped from the final view.

### 6.4 Defaults and final validation

1. Start from the base defaults: `agents = {}`, `categories = {}`, `teams = {}`, and a fully
   materialised `task` object (every `task.*` default from config-keys.md). Deep-merge the view
   over them.
2. Validate with the *full* rules (defaults applied inside any section object that is present;
   absent optional sections stay absent).
3. If that fails, prune the merged result exactly as in §4.4 step 5 (bounded to 32 passes) and
   emit **invalid-value** diagnostics whose path is the special marker for "merged config"
   (render as `merged config`). This catches values that are valid per layer but invalid once
   combined (for example a team whose required fields come from two different files).
4. If pruning cannot produce a valid result, the view falls back to the base defaults alone and
   one **validation** diagnostic with the merged-config marker is emitted ("reset to
   defaults").
5. Strip control keys again (defensive).

### 6.5 Model catalog resolution (post-step)

The view handed to mhc consumers is the §6.4 output after model catalog references have been
resolved as described in §8. Diagnostics from §8 are appended after the loader diagnostics.

### 6.6 Load result

A load returns:
- `view` (the resolved configuration object),
- `diagnostics` (loader diagnostics in production order, then §8 diagnostics),
- `layers` (raw canonicalised layers with source records),
- `sources` (every candidate's source record, loaded or not),
- `profile` (active overlay profile name if it existed).

Loading **never fails**: every problem becomes a diagnostic.

Inputs: working directory (default: process cwd), environment map (default: process
environment), harness (mhc passes `native`), explicit profile name (optional), and an
injectable read-only filesystem (exists, read, is-symlink, realpath) for tests.

---

## 7. Precedence summary (highest wins)

1. harness block inside the active profile
2. the active profile body
3. harness block (`[native]` over `[senpi]`) of the merged files
4. nearest project file
5. … farther project files …
6. user file
7. built-in defaults

Environment variables affect **only** which profile is active and where home is; there is no
per-key environment override of harness configuration.

---

## 8. Model catalog references

`models` maps an alias to `{ model: "<provider/model or other alias text>", reasoning? }`.

Resolution applies to: `agents.*.model`, `agents.*.models[*]`, `categories.*.model`,
`categories.*.models[*]`, `categories.*.fallback_models` (string or list), and
`model_profiles.*.models[*]`.

Rules:
- A string or object `model` that **exactly** equals an alias key is replaced by the alias's
  `model` value. Lookup is **one hop**: the alias's `model` is used as written even if it is
  itself another alias.
- Reasoning: an explicit reasoning on the referencing entry wins; otherwise the alias's
  `reasoning` is attached.
- String entries in a list: if the alias carries reasoning, the entry becomes the object
  `{ model, reasoning }`; otherwise it becomes the plain resolved string.
- A string `fallback_models` that names an alias with reasoning becomes a one-element list of
  that object; without reasoning it becomes the plain resolved string.
- For a definition's own `model` + `reasoning` fields: `model` is replaced; `reasoning` is added
  only when the definition has none.
- **Cycles**: an alias that reaches itself through alias→`model` links (including a direct
  self-reference) is never resolved. Each such alias yields one **catalog-cycle** diagnostic at
  path `models.<alias>.model` (message distinguishes self-reference from a longer cycle).
  Cycle names are reported in sorted order.
- **Shadowing**: a `model_profiles` id equal to a `models` alias yields a validation diagnostic at
  `model_profiles.<id>`; the alias still resolves.
- **Profile splice**: a string entry in `categories.*.models` or `agents.*.models` that equals a
  model-profile id yields a validation diagnostic at `<section>.<name>.models.<index>` (splicing
  a model profile into a chain is unsupported); the entry is left as written.
- Diagnostic order: cycles, then shadowing, then category splices, then agent splices.

---

## 9. Disabled skills (union rule)

`collect_disabled_skills(layers, harness, profile)` returns names in first-seen order, without
duplicates, gathered from every raw layer's: base `disabled_skills`; the harness blocks for the
harness (both `[native]` and legacy `[senpi]` spellings are read, regardless of which one the
caller names); and, when a profile is active, the same three scopes inside `profiles.<name>`.
Entries are trimmed; empty strings and non-strings are ignored. A later layer **cannot**
re-enable a skill an earlier layer disabled. Other harnesses' blocks are ignored.

---

## 10. Diagnostics

Each diagnostic has: a kind, the file path it is about (or the merged-config marker, or a
dotted config path for profile/catalog kinds), a human message, and a list of dotted issue
paths.

Kinds (suggested neutral identifiers): `read`, `parse`, `validation`, `unknown-keys`,
`invalid-value`, `deprecated-keys`, `profile`, `catalog-cycle`. Kind identifiers are internal;
the hot-reload validator (§14) fingerprints on kind + path + message, so keep messages stable
for the same input.

### 10.1 Display path
A file path under the home directory renders as `~/…` with forward slashes after `~`
(case-insensitive prefix comparison when home has a drive letter; home itself renders `~`).
Paths outside home render unchanged. The merged-config marker renders as `merged config`.

### 10.2 Doctor lines
For a `doctor`-style report, produce one line per item, without severity prefix (the caller adds
`WARN`):
- per issue path of an `invalid-value`: "<display path>: <key> ignored (invalid value)";
- per issue path of an `unknown-keys`: "<display path>: <key> ignored (unknown key)";
- per `read`/`parse`/`validation` diagnostic: "<display path>: not loaded (<reason>)", or for the
  merged-config marker "merged config: reset to defaults (<reason>)", where reason is "JSONC
  parse error", "unreadable", "invalid: <paths>" or "invalid config".
- `deprecated-keys` and `profile` produce no doctor line (other surfaces report them).
Exact wording is the implementer's; the facts (path, key, reason class) are mandatory. Lines
start with a fixed prefix such as `config:` so they group in the report.

---

## 11. Writer (comment-preserving edits)

Operation: apply a list of edits — *set value at path* or *remove path* — to one config file.

- Target: an explicit path, or by scope: `user` → `<home>/.omo/omo.jsonc`, else existing
  `omo.json`, else create `omo.jsonc`; `project` → `<project>/.omo/omo.jsonc`, else existing
  `omo.json`, else create `omo.jsonc`.
- Create missing directories. A new file starts as an empty object with a one-line comment
  header (wording free).
- Refuse (error, file untouched) when: the target file is a symlink; for project scope, the
  `.omo` directory is a symlink; the existing content has any JSONC error (report all errors
  with offsets).
- Before changing an existing file, write a byte-for-byte backup next to it named
  `<file>.bak.<timestamp>` with `:` and `.` in the timestamp replaced by `-`; if the name is
  taken, append `.1`, `.2`, …. A caller may pass a planned backup path; reuse it when it already
  holds identical content.
- Apply edits textually so that unrelated text, comments, key order, indentation style and line
  endings (including CRLF) are preserved; new content uses 2-space indentation and `\n` unless
  the file's existing style is detected. Trailing commas present in the file stay.
- Write atomically: temp file in the same directory with a unique name, then rename; remove the
  temp file on failure.
- Result: the path written and the backup path (if any). Errors carry path, operation (`read`,
  `parse`, `backup`, `write`) and detail.

---

## 12. Migrations

Purpose: rewrite older config shapes once, safely, so files converge to the current shape. The
loader already tolerates every legacy shape listed here (§4.5–4.6), so a migration that cannot
run never breaks loading.

### 12.1 Engine contract

Inputs: a list of **plans**, each with an id, a mode, an optional content predicate, a target
config path, and (merge mode only) source files with optional explicit backup paths, and a
transform from loaded sources to a document.

Lock:
- Lock file `<home>/.omo/.migration.lock`, created exclusively. Content: one JSON object line
  `{"leaseExpiresAt": <ms since epoch>, "pid": <int>}` followed by a newline. **This format is an
  interface fact** — other tools sharing `~/.omo` (the TypeScript OmO) use the same file, so
  keep the path and both field names.
- Lease 30 s by default; the holder renews it before each file operation.
- An existing lock is reclaimable when its lease has expired **and** either its pid is not alive,
  or it expired more than two lease durations ago. A lock file whose content is not a valid
  record counts as reclaimable.
- Reclaiming is serialised by a guard file `<lock>.guard` (same record format, 1 s lease,
  retried with short back-off of roughly 2/4/8/16/32 ms). Removal of a stale lock happens only if
  its content is still exactly what was observed.
- Up to 3 acquisition attempts. If the lock cannot be taken, the batch returns status **locked**
  and does nothing.
- The lock is released at the end, and also on any early exit.

Journal (crash recovery):
- Path `<home>/.omo/.migration-journal.json`, written atomically (unique temp + rename). It
  records: migration id, target path, planned target backup path, the document to apply and the
  mode, whether the target was written, the planned source→backup moves and which moves are
  complete, the diagnostics, and a version number (1). Field names (for interop with an
  interrupted run of the TypeScript tool): `version`, `migrationId`, `targetPath`,
  `targetBackupPath`, `targetWrite` (`{ additions, mode? }`, mode `"replace-target"` when
  applicable), `targetWritten`, `backupMoves` (`[{from,to}]`), `completedMoves` (list of `from`),
  `diagnostics`.
- At batch start, after taking the lock, an existing journal is **resumed**: if the target is not
  yet written, write it (reusing the planned backup path, or a free `.N` variant if that path is
  occupied by different content); then perform the remaining moves (a move whose source is gone
  and whose destination exists counts as done; source and destination both existing is an
  error); then delete the journal. The batch result reports `journal_resumed = true`.

Per plan:
1. Refuse duplicate source paths and sources that equal the target, the lock or the journal.
2. Read the target (missing → empty object; unparsable → error).
3. If the plan has a content predicate and it is false for the target → **skipped**.
4. "Something to migrate" means: replace-target mode → the target exists; merge mode → at least
   one source exists. If not, or the target's `_migrations` array already contains the plan id →
   **skipped**.
5. Load sources (merge mode; any source with a JSONC error aborts with an error naming it) or
   use the target itself (replace-target mode). Run the transform; it must return an object.
6. Build the new document:
   - merge mode: add transform output to the target **without clobbering** — keys absent in the
     target are added, nested objects merge, and a key present in both with non-object values
     keeps the target's value and records a "skipped: <path> legacy=<v> kept=<v>" style
     diagnostic;
   - replace-target mode: the transform output replaces the target content (edits are computed
     as a structural diff so the writer preserves comments where keys are unchanged);
   - in both modes, any key named `codegraph` at the top level, inside any harness block, inside
     any profile, or inside a profile's harness blocks is removed (retired setting) with a
     "removed … (retired configuration)" diagnostic;
   - append the plan id to `_migrations` (create the array if missing; if `_migrations` exists
     but is not an array of strings → validation error).
7. Validate the new document with the **full strict** rules. Failure → error (nothing written).
8. Dry-run mode stops here with status **planned** and a preview (target path, document, moves).
9. Plan source backups: each existing source moves to its explicit backup path (error if that
   path exists), or to `<source>.bak.<url-encoded plan id>` with `.1`, `.2`… suffixes if taken.
   Backup paths must not collide with sources, target, lock or journal.
10. Plan the target backup `<target>.bak.<ISO-8601 timestamp with : and . replaced by ->`
    (`.N` suffix if taken) if the target exists.
11. Write the journal; write the target through the writer (§11, with the planned backup);
    mark the journal; move each source to its backup (creating directories), updating the
    journal after each move; delete the journal. Status **migrated**.

Any error aborts the whole batch (later plans do not run), releases the lock, and is reported.
The batch result: status (`completed` or `locked`), `journal_resumed`, the resumed run's target
backup path, and per-plan results (status, diagnostics, preview with backup moves, target
backup path).

Test hooks: injectable clock, pid, process-liveness check, filesystem (with "replace if content
matches" and "remove if content matches" primitives), and a callback at each durable boundary
(journal written, target written, target recorded, source moved, source recorded) that may
return an error to simulate a crash.

### 12.2 Plan set run at startup

Discovery inputs: working directory, environment, home directory, platform (POSIX or Windows
path semantics; WSL is Linux).

Targets for the in-place plans: the existing user file (`omo.jsonc` else `omo.json` in
`<home>/.omo`), plus the existing config file of `<dir>/.omo` for each directory from the working
directory upward (inclusive of home when the walk starts inside home; to the root otherwise),
plus the target of every import plan below. Targets are de-duplicated by canonical path (case-
insensitive on Windows), keeping first-seen order.

Four replace-target plans exist per target. Execution order: the first plan for every target
(in target order), then the second plan for every target, and so on. Their ids are stored in user
files and are therefore **interface facts that must be reproduced exactly**:

1. `2026-08-reasoning-unification` — no predicate (runs once on every existing target):
   - normalise every model-bearing object as in §4.5 and canonicalise model strings, in the
     top-level sections, inside `[senpi]` and `[codex]` blocks, inside the `[opencode]` block
     (for `[opencode]`, keep `maxTokens`, `providerOptions`, `textVerbosity` as written), and
     inside each profile (one level; profiles' harness blocks included);
   - **chain folding**: for a category that has `fallback_models`, and for an agent that has
     both `model` and `models` (or has `fallback_models`), replace them with a single `models`
     list = [primary entry built from `model` plus its reasoning/provider settings] + [the
     agent's existing `models`, agents only — a category's own `models` is discarded] + [the
     `fallback_models` entries]; the primary entry exists only when `model` is a string; then drop `model`,
     `fallback_models`, `reasoning`, `variant`, `reasoningEffort`, `provider_options`,
     `providerOptions`, `thinking`, `textVerbosity` from that definition;
   - catalog entries in `models` are normalised to the canonical object/string form;
   - when a definition carries more than one of `reasoning` / `reasoningEffort` / `variant`, keep
     the first in that order and add a diagnostic "conflict: <path> dropped … kept …";
   - an entry that cannot be normalised into a valid model reference aborts the batch with an
     error naming the issues.
2. `2026-09-category-deep-split` — predicate: the file contains a legacy category name (§4.6).
   Transform: the §4.6 category rename, with one diagnostic per rename.
3. `2026-09-harness-native-rename` — predicate: the file contains a `[senpi]` block (top level or
   in a profile). Transform: the §4.6 harness rename.
4. `2026-09-subscription-provider-rename` — predicate: some provider id needs renaming.
   Transform: walk every string value **and every object key** in the document; a value/key
   that is exactly a legacy provider id, or starts with `<legacy id>/`, is rewritten:
   `claude-sdk-oauth` → `anthropic-subscription`, `openai-codex` → `chatgpt-subscription`.
   (`openai` and `anthropic` are *not* renamed.) Diagnostics use `$`-rooted paths, e.g.
   `$.categories.quick.model: <from> renamed to <to>`. Everything else is untouched.

Two **import plans** (merge mode) bring configuration from older products into the harness
file. Ids (interface facts): `2026-07-opencode-config-unification`,
`2026-07-codex-config-jsonc`. Their output lands only in the `[opencode]`, `[codex]` and
`[native]` blocks and in `profiles.*.[opencode]`, `$schema`, `legacy_migrations`. Because mhc
reads only `[native]`:
- `2026-07-codex-config-jsonc`: source `<home>/.omo/config.jsonc` (and its
  `config.jsonc.migrations.json` sidecar). Copies its `[opencode]` and `[codex]` blocks as-is and
  its `[senpi]` block (else its `[omo]` block) into `[native]` of the user file; a conflict
  diagnostic when both `[senpi]` and `[omo]` exist. Source backups go to
  `<home>/.omo/migration-backup-<timestamp>-opencode-config/<home-relative path>`.
  **Recommendation: implement** (it carries real `[native]` settings).
- `2026-07-opencode-config-unification`: imports `oh-my-openagent.json[c]` /
  `oh-my-opencode.json[c]` from OpenCode config roots into `[opencode]` only. mhc never reads
  `[opencode]`. **Recommendation: do not implement in the MIT build** (open question Q3); if a
  user still has such files, the TypeScript tool migrates them.

Import plans run before the four in-place plans so the in-place plans see their output.

### 12.3 Startup migration wrapper

`run_startup_migration(cwd, env, home)`:
- empty home → result with error "no home directory available" (wording free);
- run the batch with the §12.2 plans (not dry-run);
- a discovery failure → error result;
- batch error → error result with the message;
- status locked → error result stating that another process holds the migration lock;
- otherwise: `migrated_from` = sorted unique list of every source path moved by a migrated
  plan; `journal_resumed`; per-plan results (for their diagnostics).

---

## 13. Startup notices (extension `config-notices`)

On registration (once per process start of the extension):
1. Determine home (`HOME`, else `USERPROFILE`); if present, make sure both env entries exist in
   the env map handed to the loader.
2. Run the startup migration (§12.3), then load the configuration (§6) for the session cwd with
   harness `native`.
3. Compute notices, in this order:
   - migration error → warning naming the error; else non-empty `migrated_from` → info listing
     the migrated paths; else `journal_resumed` → an info notice stating that an interrupted
     migration was completed;
   - if any plan produced diagnostics → one warning joining them with `; `;
   - if the load produced diagnostics → one warning joining their messages with `; `;
   - optional provider-specific advisory hook: a caller-supplied predicate over model selectors
     found in `categories.*` / `agents.*` (`model`, `models[*]` string or `.model`,
     `fallback_models`) may produce one warning listing offending selectors with their config
     paths (used today for a provider that serves only some lanes of a model; see
     model-selection.md Q6). Inert when no predicate is supplied.
4. On the first `session_start` event only, deliver each notice: through the UI notification
   API (info/warning) when a UI exists, otherwise to the diagnostic log. Never deliver twice per
   extension instance.

Notice text must be the implementer's own; prefix with the product name (`mhc`), not "OmO".

Injectable for tests: the migration runner and the config loader.

---

## 14. Hot reload (extension `config-hot-reload`)

mhc already ships the MIT host side (`maho-ext-config-reload`) and the registration API in
`maho-ext-api` (`register_config_watch`, `RegisteredConfigWatch`, `ConfigWatchTargetSpec`,
`ConfigWatchValidation`, the `config-watch:register` / `:ready` / `:reloaded` / `:rejected`
event channels). This extension supplies the targets and the validator for the harness config.

### 14.1 Registration
- id: a stable identifier for this registration (today `omo`; the host keys on it — keep the
  value `omo` for compatibility unless the host is changed in the same PR); display name e.g.
  `.omo config`.
- Targets are directory watches with filter globs (paths relative to the watched directory,
  written with a leading `/`):
  - user: if `<home>/.omo` is a directory → watch it with filters `/omo.jsonc`, `/omo.json`;
    else, if `<home>` is a directory → watch `<home>` with filters `/.omo`, `/.omo/omo.jsonc`,
    `/.omo/omo.json` (so creating the user file is noticed);
  - project: for each ancestor of cwd, starting with cwd itself (up to 128, and up to and
    including home when cwd is inside home): if
    `<ancestor>/.omo` is a known project config directory (from §2.3) or is a real (non-symlink)
    directory → watch it with `/omo.jsonc`, `/omo.json`; and watch every ancestor itself with
    `/.omo`, `/.omo/omo.jsonc`, `/.omo/omo.json` (so a new `.omo` is noticed);
  - skip a watch whose filesystem is a 9P/Plan 9 mount (Linux `statfs` type `0x01021997`, used by
    WSL for Windows drives) because events are unreliable there;
  - **protected paths**: drop any target that is inside, or whose filters would match inside,
    `<agent dir>/auth.json`, `<agent dir>/sessions`, `<agent dir>/logs` (agent dir as resolved by
    `maho-core`).
- If no user-level target could be registered, log one warning that creating the user file
  later requires a reload.
- Subscribe to the reloaded/rejected channels **before** publishing the registration (the host
  may reject synchronously).

### 14.2 Validation verdict
A validator object lives for the extension's lifetime:
- At creation, load the configuration and record the **baseline**: the fingerprints (hash of
  kind, path, message) of all diagnostics except `deprecated-keys` (deprecations never block a
  reload).
- On each change set (list of changed paths): reload, recompute fingerprints (ignoring
  deprecated-keys). A diagnostic is **attributable** to the change when its path is the
  merged-config marker, or its kind is catalog-cycle, or its path lies under a changed path, or
  it belongs to the same `.omo` directory (user `.omo`, or the nearest `.omo` ancestor) as a
  changed path.
- New (not in baseline) attributable diagnostics are added to an **unresolved** list; entries
  whose diagnostic disappeared are removed from it.
- Unresolved non-empty → verdict **rejected** with the messages of the unresolved diagnostics
  (the host keeps the previous configuration). Otherwise verdict **ok** and the baseline becomes
  the current fingerprint set.
- Consequence: an error stays "sticky" until its source is repaired; unrelated pre-existing
  problems never block a reload.

### 14.3 Lifecycle
- On `config-watch:ready`, re-publish the registration on a fresh task (never synchronously).
- On `config-watch:rejected` for this id: log a warning with paths and errors; recompute targets
  (a repair may have created a new `.omo`), update the registration; re-publish on a fresh task.
  Re-publishing is capped at **3** attempts per distinct target set (the counter resets when the
  target set changes); when exhausted, log a warning and stop.
- On `config-watch:reloaded` for this id: log an info with the paths.
- If another extension instance publishes a registration with the same id, this instance logs a
  warning and stands down (unsubscribes, stops re-publishing).
- On session shutdown: unsubscribe and cancel pending re-publishes.

---

## 15. Environment variables (summary)

| Variable | Effect |
| --- | --- |
| `HOME`, then `USERPROFILE` | home directory for all paths |
| `OMO_PROFILE` | active overlay profile (2nd priority after an explicit caller value) |
| `OCX_PROFILE` | active overlay profile (3rd) |
| `OPENCODE_CONFIG_DIR` | profile name taken from a `…/profiles/<name>` tail (4th) |
| `XDG_CONFIG_HOME`, `APPDATA`, `WSL_DISTRO_NAME`, `WSL_INTEROP` | only for the OpenCode import discovery (if implemented, §12.2) |

---

## 16. Required API surface and consumers

The implementer provides (names are suggestions):

| Capability | Needed by (today's call site → future MIT consumer) |
| --- | --- |
| `load(options) -> LoadResult` with `view`, `diagnostics`, `layers`, `sources`, `profile`; options: cwd, env map, harness, profile, injectable fs | `maho-cli` doctor (`cli/doctor.rs` `loaded_config`), task launch (`cli/task_runners.rs` `resolved_omo_config` uses only `view`), model profile (model-selection.md §6), startup notices, hot reload validator, Phase 2 task/memory/team/LSP/formatter, Phase 3 gateway/git attribution/side panel |
| `load_native(cwd, env)` convenience = load with harness `native` + catalog resolution (§6.5) | replaces `maho_omo_config_resolution::load_senpi_omo_config`; consumers: doctor, task runners, model profile, startup, watch, fallback-architect replacement (Phase 3 public API) |
| `doctor_lines(diagnostics, home) -> Vec<String>` and `display_path(path, home)` | `cli/doctor.rs` `config_lines` |
| `resolve_paths(cwd, env, fs) -> Vec<Candidate>` and `project_paths_farthest_first(cwd, home, fs, account_home)` | hot-reload targets |
| `user_config_dir(env)`, `user_config_path(env)`, `home_dir(env)` | hot reload, writer, migrator |
| `resolve_profile_name(env, explicit)` | load, tests |
| `collect_disabled_skills(layers, harness, profile)` | Phase 3 skill loader |
| `resolve_catalog_references(view) -> (view, diagnostics)` | load_native |
| `canonical_model_string`, `normalize_legacy_model_fields`, reasoning helpers | migrator, model-selection |
| `update_config(edits, scope/target, …) -> {path, backup}` | migrator; future `/settings`-style UI for harness keys |
| `run_migrations(plans, options) -> BatchResult`, `startup_migration_plans(discovery)`, `run_startup_migration(cwd, env, home)` | `config-notices` |
| `ConfigNotices` extension (registers on `session_start`) | mounted by the MIT replacement of the omo glue (`omo_mount.rs` / `maho-omo` component list today registers `config-startup`) |
| `ConfigHotReload` extension + `Validator::validate(changed) -> Ok or Rejected{errors}` | mounted alongside; consumed by `maho-ext-config-reload` through `maho-ext-api` |
| JSON Schema export of config-keys.md | regenerate `assets/omo.schema.json` (Phase 4) |

`maho-cli` in the MIT profile must be able to call `load_native` and `doctor_lines` without the
`omo` feature once these crates exist.

---

## 17. Test cases (given / when / then)

Paths below use a temporary home `H` and project tree under it unless stated.

Locations
1. Given `H/.omo/omo.jsonc` and `H/.omo/omo.json` both exist, when loading, then only the
   `.jsonc` is a source and loaded.
2. Given only `H/.omo/omo.json`, then it is the user source and is loaded.
3. Given cwd `H/a/b/c` with `.omo/omo.jsonc` in `H/a` and `H/a/b/c`, then sources are
   [user, `H/a/...`, `H/a/b/c/...`] and a key set in both projects takes the `H/a/b/c` value.
4. Given cwd `/tmp/x/y` (outside home) with `/tmp/.omo/omo.jsonc`, then it is read.
5. Given cwd = `H`, then no project source exists (home is a boundary).
6. Given `HOME` overridden to `T` while the account home `A` is an ancestor of cwd, then the walk
   stops at `A` and `A/.omo` is not read as a project layer.
7. Given `H/p/.omo` is a symlink to a directory with `omo.jsonc`, then `H/p` contributes nothing.
8. Given `H/p/.omo/omo.jsonc` is a symlink, then it is skipped.
9. Given a directory chain deeper than 256 levels, then the walk stops after 256.
10. Given `XDG_CONFIG_HOME` points at a dir with `omo.jsonc`, then it is never read.

Format and validation
11. Given `{ // c\n "agents": {"a": {"model": "x/y",}}, }`, then it loads (comments + trailing
    commas).
12. Given `{"agents":` (truncated), then not loaded, one parse diagnostic.
13. Given `[]` as root, then not loaded, validation diagnostic "root must be an object".
14. Given `{"bogus": 1, "categories": {"quick": {"model": "a/b"}}}`, then loaded, `bogus` removed,
    one unknown-keys diagnostic with `bogus`, `categories.quick.model` kept.
15. Given `{"agents": {"x": {"model": 5, "description": "d"}, "y": {"model": "a/b"}}}`, then
    `agents.x.model` dropped with an invalid-value diagnostic, `agents.x.description` and
    `agents.y` kept.
16. Given every value of a file is invalid, then the file is rejected with one validation
    diagnostic and contributes nothing, while a valid user file still applies.
17. Given `{"__proto__": {"a": 1}, "models": {"m": {"model": "a/b"}}}`, then `__proto__` removed
    with unknown-keys diagnostic, `models.m` kept.
18. Given a category with `"variant": "high"` and no `reasoning`, then the view entry has
    `reasoning: "high"` and no unknown-key diagnostic.
19. Given a model string `"openai/gpt-x (high)"`, then the view has `"openai/gpt-x:high"`.
20. Given `"gpt-x:max"` (no provider), then the string is kept unchanged.

Legacy names
21. Given `categories.deep`, then the view has `categories.deep-low` and a deprecated-keys
    diagnostic; given both `deep` and `deep-low`, then `deep-low` is kept unchanged and `deep`
    reported as ignored.
22. Given `[senpi]: {"categories": {...}}` only, then it applies for harness `native`; given both
    `[senpi]` and `[native]` setting the same key, then `[native]` wins.

Merge / view / profiles
23. Given user `{"disabled_skills": ["a"]}` and project `{"disabled_skills": ["b"]}`, then the
    view's `disabled_skills` is `["b"]` but `collect_disabled_skills` returns `["a","b"]`.
24. Given `profiles.fast.categories.quick.model = "p/m2"` and `OMO_PROFILE=fast`, then
    `categories.quick.model` is `p/m2` and result `profile = "fast"`.
25. Given an explicit profile `slow` and `OMO_PROFILE=fast`, then `slow` is active.
26. Given `OPENCODE_CONFIG_DIR=/x/profiles/work/`, then the active profile is `work`; given
    `/x/profiles/a/b`, then none.
27. Given `OMO_PROFILE=missing`, then one profile diagnostic at `profiles.missing`, base view
    used, result `profile` absent.
28. Given base, `[native]`, profile and profile `[native]` each setting `telemetry.enabled`
    (false, true, false, true), then the view value is `true` (profile's harness block wins).
29. Given no file at all, then the view has `agents={}`, `categories={}`, `teams={}`, a complete
    `task` object with defaults, and no `profiles` or harness keys.
30. Given two project files each providing half of a team's required fields such that the merged
    team is still invalid, then the team is dropped from the view with an invalid-value
    diagnostic whose path renders `merged config`.

Catalog
31. Given `models.fast = {model: "p/m", reasoning: "high"}` and `agents.w.models = ["fast"]`, then
    `agents.w.models = [{model: "p/m", reasoning: "high"}]`.
32. Given `models.a = {model: "b"}`, `models.b = {model: "a"}`, then two catalog-cycle diagnostics
    (`models.a.model`, `models.b.model`) and references to `a` or `b` stay unresolved.
33. Given `models.x = {model: "models-y"}` and `models.models-y = {model: "p/q"}`, then a
    reference to `x` resolves to `models-y` (one hop only).

Doctor lines
34. Given an invalid-value at `task.max_depth` in `H/.omo/omo.jsonc`, then a line containing
    `~/.omo/omo.jsonc`, `task.max_depth` and "invalid value".
35. Given a Windows home `C:/Users/Me` and a path `c:\Users\me\.omo\omo.jsonc`, then it displays
    as `~/.omo/omo.jsonc`.

Writer
36. Given a tab-indented CRLF file with a block comment between two keys, when adding
    `agents.new` and removing `telemetry`, then the comment, CRLF endings, tab indentation and
    other keys are preserved, and a backup with the original bytes exists.
37. Given a file with a JSONC error, when editing, then error with operation `parse` and the file
    is unchanged.
38. Given the target is a symlink, then the writer refuses.

Migrations
39. Given a user file with a category `{model: "a/b", fallback_models: ["c/d"]}` and no
    `_migrations`, when startup migration runs, then the file has `models: ["a/b", "c/d"]`, no
    `model`/`fallback_models`, `_migrations` contains `2026-08-reasoning-unification`, and a
    `.bak.<timestamp>` copy holds the original.
40. Given the same file already containing that marker, then the plan is skipped and the file is
    byte-identical.
41. Given a file without `deep` / `[senpi]` / legacy providers, then plans 2–4 are skipped and
    add no marker.
42. Given `"model": "openai-codex/gpt-x"` and a key `"claude-sdk-oauth"` in a map, then they
    become `chatgpt-subscription/gpt-x` and `anthropic-subscription`; `"openai/gpt-x"` is
    untouched.
43. Given a live process holds a fresh lock, then the batch returns locked and the startup
    notice says migration is already running; nothing is written.
44. Given a stale lock whose pid is dead, then it is reclaimed and migration proceeds.
45. Given a crash simulated right after the target is written, when the next startup runs, then
    the journal is resumed, remaining moves done, journal deleted, `journal_resumed = true`.
46. Given a target containing an unknown key, then the reasoning plan's strict validation fails,
    the batch reports an error, and the file is unchanged.
47. Given `codegraph` inside `[native]`, then the migrated file lacks it and a "retired" diagnostic
    is reported.
48. Given `H/.omo/config.jsonc` with a `[senpi]` block, then the user `omo.jsonc` gains
    `[native]` with that content, the source moves under
    `H/.omo/migration-backup-<ts>-opencode-config/`, and `_migrations` contains
    `2026-07-codex-config-jsonc`.

Startup notices
49. Given a startup where one legacy source was migrated and `OMO_PROFILE` names a missing
    profile, then on first session start the UI receives an info notice listing the migrated
    source path and a warning containing the missing-profile diagnostic; a second session start
    in the same process delivers nothing.
49b. Given a project file with an unknown key, then the in-place migration of that file fails
    strict validation (§12.1 step 7), the startup shows one migration-error warning and one
    load-diagnostics warning naming the key; plans that ran before the failing one (e.g. the user
    file's reasoning plan) keep their effect, later plans do not run.
50. Given no UI (print mode), then notices go to the diagnostic log.

Hot reload
51. Given a valid config, when the user file gains an unknown key, then the verdict is rejected
    with that message; when the key is removed, then ok.
52. Given a pre-existing invalid project file, when the user file changes validly, then ok (not
    attributable).
53. Given a change that only renames `deep` → `deep-low`, then ok (deprecations never reject).
54. Given `<home>/.omo` does not exist, then `<home>` is watched with `.omo` filters; given home
    itself is missing, then a single "reload required" warning is logged.
55. Given the agent dir is `<home>/.omo/agent`, then no target watches inside `auth.json`,
    `sessions` or `logs`.
56. Given the host rejects four times in a row with an unchanged target set, then only three
    re-publishes happen.

---

## 18. Open questions

- **Q1** Keep reading `OCX_PROFILE` and `OPENCODE_CONFIG_DIR` (other products' variables)? They
  are cheap compatibility, but name third-party ecosystems. Proposed: keep, document.
- **Q2** The registration id `omo` and the `~/.omo` paths keep the OmO brand on disk. Plan §0 keeps
  `~/.omo` format compatibility; confirm the id may stay `omo`.
- **Q3** Drop the OpenCode import plan (`2026-07-opencode-config-unification`) from the MIT build?
  Its output is never read by mhc. Recommended: drop.
- **Q4** Journal interop: implementing the same journal field names lets mhc finish a migration
  the TypeScript tool started. Alternative: use a different journal file name and only share the
  lock. Proposed: share both (format above).
- **Q5** The full-schema validation in the migrator (§12.1 step 7) makes a file with one typo
  un-migratable until fixed. Keep (safety) or relax to the lenient loader rules? Proposed: keep,
  and make the startup notice point at the offending key.
- **Q6** Agent-directory env lookup order differs between docs/configuration.md
  (`MAHO_`, `SENPI_`, `PI_`, `OMO_`) and the current omo code (`MAHO_`, `OMO_`, `SENPI_`, `PI_`).
  Hot reload should simply use `maho-core`'s resolver; confirm which order is authoritative.
- **Q7** Defaults for absent optional sections: the spec says only `task` (plus empty
  `agents`/`categories`/`teams`) is materialised when absent. Confirm with a behavioural test
  against the current binary (`mhc doctor`/print of resolved config) before freezing.
- **Q8** Should doctor/notices report `deprecated-keys` somewhere? Today they only appear as a
  startup warning (through the "load diagnostics" notice) and are excluded from doctor lines.

---

## Leak self-check (spec writer)

- No Rust/TS code from `crates/omo/**` is reproduced; JSON snippets are user-config examples.
- Struct/field names in the SUL crates are not reused as API names; the API table uses new,
  descriptive names. Diagnostic kind strings are proposed neutral identifiers (internal only).
- Verbatim strings kept on purpose (interface facts): file names (`omo.jsonc`, `omo.json`,
  `.migration.lock`, `.migration-journal.json`, `config.jsonc`, backup name patterns), config
  keys, enum values, env var names, migration ids stored in `_migrations`, lock/journal JSON
  field names, provider ids in the rename rule, the 9P filesystem magic number.
- Notice/diagnostic/log wording is described by required facts, not copied.
- No OmO-original feature is specified.
