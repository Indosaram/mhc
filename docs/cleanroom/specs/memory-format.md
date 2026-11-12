# Legacy memory store — on-disk format (spec for the MIT migrator)

Phase 2, step 3 of the MIT migration. This document describes the persistent memory data that
earlier mhc builds (the `omo` feature, "memory-core") leave on a user's disk, so that the MIT
memory crate (`crates/maho-memory`) can import it into its SQLite store. It is the input for the
migrator only; the new store has its own format (mnemopi's SQLite schema, see
`crates/maho-memory/PROVENANCE.md`).

## Sources this spec was written from

Earlier revisions of this document were written without access to a real store and therefore
described a flat layout (`<home>/.git`, `<home>/system/…`) that does not exist. That premise was
wrong and the importer could not read a real store. This revision was written **from observed
data**: a live store on a developer machine (`~/.omo/memory`, 274 identities, 2,218 Markdown
files, 6,577 commits) plus the authoritative layout constants in
`crates/omo/memory-core/src/identity/layout.rs`. No SUL source text is reproduced; the facts
below are the observable on-disk contract.

| Fact | Where it is stated / observed |
| --- | --- |
| Store root is `~/.omo/memory`, overridable with `MAHO_MEMORY_HOME` | `docs/configuration.md`; `identity/layout.rs` (`MEMORY_ROOT_ENV_VAR`, `default_memory_root`) |
| Content lives at `<root>/agents/<safe-id>/repo/`, not at the root | `identity/layout.rs` (`AGENTS_DIRNAME="agents"`, `REPO_DIRNAME="repo"`); observed on disk |
| Each identity has a sibling `<root>/agents/<safe-id>/runtime/` that is **not** memory | `identity/layout.rs` (`RUNTIME_DIRNAME`, 14 `RUNTIME_SUBDIRNAMES`); observed |
| Content homes are `system/`, `reference/`, `notes/`, `people/`, `skills/` | `audit/audit.rs` orphan check (list); `memfs/paths.rs::is_memory_content_path` (list differs — see below) |
| The path predicate and the audit list disagree | `memfs/paths.rs` accepts only `system/`, `reference/`, `people/` and `skills/<slug>/SKILL.md`; `audit/audit.rs` accepts `system, reference, notes, people, skills` plus `ARCHIVE.md`/`README.md`; neither lists `project/` |
| `project/` is a content home in practice | observed: 193 files under `project/` in one live identity |
| `skills/<slug>/SKILL.md` is content; other files under `skills/` are not | `memfs/paths.rs::is_memory_content_path` |
| The repo is a git work tree with auto-commit; history is real and used | observed: `.git` 56.8 MB, 6,577 commits, 4 of 273 identities had empty repos |
| `system/human.md` is the primary human's card: `description` is the display name, `kind: person`, `aliases` optional | observed frontmatter in a live store; `memfs/frontmatter.rs` fields |
| Facts are written to `notes/facts/<month>.md` as list items, plus free-form fact files | observed: `notes/facts/2026-11.md` (list items) and `notes/facts/<topic>.md` |
| People profiles live under `people/<slug>/` with `card.md` + `observations.md`; gated by `memory.people.enabled` (default false) | `docs/divergences.md` SD-7; observed |
| Config keys that shape the store | `docs/cleanroom/specs/config-keys.md` § `memory` |

## Directory layout

```
<memory root>/                    MAHO_MEMORY_HOME, else ~/.omo/memory
  agents/
    <safe-id>/                    one identity ("agent"); <safe-id> is a filesystem-safe name
      repo/                       THE memory content: a git work tree
        .git/                     auto-commit history; may have a remote (memory.sync.remote)
        system/
          human.md                primary human card (identity; description = display name)
          persona.md              agent persona
          self-aware.md           behavioural observations about the agent
          boundaries.md           what the user forbade, in their words
          agreements.md           standing agreements ledger
          identity.md             optional identity card (created only when one emerges)
          *.md                    other system cards
        notes/
          facts/
            <YYYY-MM>.md          one facts file per month, list items
            <topic>.md            free-form fact files
          *.md
        people/
          <slug>/
            card.md               profile card
            observations.md       dated observations
        project/
          <slug>.md               project notes and task records
        reference/
          self/observations.md    reflection observations
        skills/
          <slug>/SKILL.md         agent skills (only SKILL.md is memory content)
      runtime/                    NOT memory: locks, transcripts, reflection, queues, worktrees
  runtime/                        store-wide runtime (when present)
  transient-runs/                 NOT memory
```

Rules the migrator must honour:

- **Resolve content roots first.** If `<root>/agents/` exists, each `<root>/agents/<id>/repo/` is a
  content root. Otherwise the store is the older flat layout and `<root>` itself is the content
  root. Both layouts must import.
- **Import the union of the content homes the SUL code recognises**, because the two SUL lists
  disagree and neither includes `project/`, which real stores use:
  `system/`, `notes/`, `people/`, `project/`, `reference/`, and `skills/<slug>/SKILL.md`.
  Importing a note the audit would call an orphan loses nothing; skipping `project/` loses real
  memory.
- **Paths are content-root relative.** A file at `agents/<id>/repo/system/human.md` has the memory
  path `system/human.md`. Comparing store-root-relative paths is the defect that made every real
  file import as an anonymous note.
- **`runtime/` is never memory**, at the store root or beside an identity's `repo/`.
- **Identities are kept apart.** The same relative path in two identities is two different
  memories; imported ids and `legacy_path` metadata carry the identity label.
- **A flat `--from <dir>` keeps the historical permissive behaviour**: any Markdown file under it
  imports (except `runtime/` and `transient-runs/`), so importing an arbitrary notes folder still
  works.
- `<month>` is a calendar month; only a `YYYY-MM` prefix is used as a date hint, and any file name
  is accepted.
- The tree is the user's own: files may be added, renamed or hand-edited. No fixed file set may be
  assumed; empty, oversized and malformed files must be tolerated.
- Directories starting with `.` (`.git`, editor and tool state) are not memory content.
- Non-Markdown files are not memory content (they are counted and skipped).

## File format

Every memory file is UTF-8 Markdown, optionally starting with a YAML frontmatter block:

```
---
description: <one line>        # for system/human.md: the human's display name
aliases: [<name>, ...]         # optional, list of alternative names (system/human.md)
<other keys>: ...              # undocumented; preserved as metadata, not interpreted
---
<Markdown body>
```

- Frontmatter is present only when the file's first line is exactly `---`; it ends at the next
  line that is exactly `---`. A file whose frontmatter does not close is read as body-only (the
  whole text is body), because no closing delimiter means no reliable point to cut at, and a
  warning is reported.
- A file whose frontmatter *closes but does not parse* as a YAML mapping is cut at the closing
  delimiter: the frontmatter is dropped and the body after it is imported, with one warning. The
  frontmatter text is **never** promoted to memory content. Real stores contain such files
  (`description: Foo: bar` is not valid YAML), and reading them whole turned the YAML delimiters
  and the description line into a memory that could be recalled and searched — 138 of 18 454
  imported blocks on a real store. Only the body carries meaning to a reader; the unparseable
  header is a formatting defect in the legacy file, not a memory.
- Frontmatter blocks may be **stacked**: a real file can open with two complete `---` … `---`
  blocks before the body. Each leading block is stripped in turn, and the first one that parses as
  a mapping becomes the file's frontmatter. Stripping only the first block leaves the second one at
  the head of the body, which reintroduces the leak above for 69 blocks on a real store.
- The body is free Markdown. The documents call the store a "block store": the unit of memory is
  a block of text inside a file. The block boundaries are not documented, so the migrator uses the
  Markdown structure as the block boundary (next section).

## How the migrator reads blocks

This is a design decision of the MIT side, not a property of the legacy format:

1. The body is split into sections at ATX headings (`#` … `######`). The heading text becomes the
   block's `section` metadata; the heading line itself is not a memory.
2. Inside a section, each top-level list item (`- `, `* `, `+ ` or `1. ` at column 0, with its
   indented continuation lines) is one block. Facts files are expected to be lists, so each fact
   becomes one memory.
3. Any other run of non-blank lines (a paragraph, a code fence kept whole) is one block.
4. Blocks are trimmed; empty blocks are dropped. A block longer than 8,000 characters is kept
   whole (mnemopi stores long content; recall previews clip it).
5. A file whose body has no blocks but has a frontmatter `description` yields one block with the
   description.

## Mapping into the new store

Each block becomes one row of mnemopi `working_memory` in the **global (default) bank**, because
the legacy store is one per user, not per project:

| New field | Value |
| --- | --- |
| `id` | first 16 hex chars of SHA-256 of `<relative path>\0<block index>\0<block text>`; stable, so re-running the import skips rows that already exist |
| `content` | the block text; for `system/human.md`, blocks are prefixed with `About the user (<description>): ` when a description is present |
| `source` | `import` |
| `veracity` | `imported` |
| `trust_tier` | `IMPORTED` |
| `scope` | `global` |
| `importance` | `0.9` for `system/`, `0.8` for a person card, `0.7` for `notes/facts/`, other `notes/`, `project/` and `reference/`, `0.6` otherwise |
| `memory_type` | `identity` for `system/human.md`, `fact` for `notes/facts/`, `person` for `people/`, `note` otherwise |
| `timestamp` | the month hint (`YYYY-MM-01T00:00:00.000Z`) when the file name starts with `YYYY-MM`, else the file's modification time, else the import time |
| `metadata_json` | `{"legacy_path": <identity label + relative path>, "section": <heading or null>, "block": <index>, "frontmatter": <mapping or null>, "importer": "maho-memory legacy-v1"}` |
| `consolidated_at` | the import time (durable; never trimmed as scratch) |

People profiles: files under `people/` are imported only when the caller opts in
(`--include-people`, or `memory.people.enabled: true` in the merged omo config). Otherwise they
are counted as skipped. This keeps the privacy default of SD-7: data about third parties is not
copied into a new place unless the user enabled people memory.

## Migrator behaviour

- Input: the memory home (explicit `--from`, else `MAHO_MEMORY_HOME`, else `~/.omo/memory`).
  A missing directory is not an error: the report says there is nothing to import.
- Content roots are resolved per the layout rules above; a store with `agents/` imports every
  `agents/<id>/repo/` and never descends into `runtime/`, and a flat store imports its own root.
- Read-only on the legacy tree. It never writes, moves, commits or deletes legacy files, and it
  does not run git.
- Idempotent: rows whose id already exists are skipped and counted as `skipped_existing`.
- `--dry-run` parses and reports without writing.
- Report (text and JSON): files scanned, files imported, blocks imported, skipped existing,
  skipped people files, skipped non-Markdown files, warnings (path + reason).
- Symbolic links are not followed (a link is reported and skipped).
- Files larger than 4 MiB are skipped with a warning.
- `--history` replays each file's git revisions (off by default: it runs git per file). The
  working tree is what a reader sees now; history is what lets the store answer "what did this say
  last month". Each commit that changed a file contributes its blocks with that commit's own
  timestamp, immediately closed by the next revision, so a recall for now returns the current text
  and a recall for a past instant returns the text that was true then. Measured on one real
  identity repo: 129 current blocks plus 399 recovered revisions. Four rules the importer must
  keep, each learned from a defect:
  - Commits are ordered by commit time, not graph order: after a rebase the two disagree, and
    closing a revision by its graph successor records a `valid_until` earlier than the row's own
    `timestamp`.
  - A revision is closed by a successor that is not earlier than it. Commit times have one-second
    precision, so consecutive revisions of a fast-moving file routinely share a second.
  - A revision byte-identical to the one before it is dropped: a commit touching other files still
    appears in this file's history, and importing unchanged text stores the same memory twice.
  - With history on, the newest revision *is* the current file, so the working-tree pass covers
    only files whose history could not be read (untracked files, or a root outside a work tree).
- Reading history back out: `mhc memory search --at <iso> <query>` answers against that instant
  rather than now (`MemoryContext::recall_at`; the same `RecallOptions::valid_at` the library has
  had all along). The argument is parsed and re-rendered in the storage shape before it reaches
  SQL, because rows are `YYYY-MM-DDTHH:MM:SS.mmmZ` and a date-only or offset argument compared as
  text lands on the wrong side of a deadline; an unparseable argument is an error rather than a
  silent fallback to now. The listing header names the resolved instant, so
  `2026-06-01T00:30:00+09:00` reports `2026-05-31T15:30:00.000Z`.
    Importing both stores every block twice under two different timestamps.
- The repo is resolved per file, not per content root: `--from` may point at an identity directory
  or the store root, neither of which is itself a work tree, while the repo is `agents/<id>/repo/`
  below it.

### 1.4 Cost of a full history import

`--history` is off by default because it is not free, and the cost is entirely in git subprocesses,
not in SQLite. Measured on one real identity repo of 260 Markdown files:

| pass | wall time |
|---|---|
| working tree only | 1.3 s |
| with history | 38 s |

The delta is one `git log` and one `git show` per (file, revision): 753 revisions, so roughly 1,000
subprocess spawns. Resolving the work tree once per root rather than per file was measured and
saved 0.7 s of 38 s, so it was not kept -- the spawns are the cost, not the repository lookup.

Across a whole real store (275 identity repos, 2,212 Markdown files, 6,092 Markdown-touching
commits) the result is 1,427,740 rows, of which 23,046 are live now; the database reaches about
1.6 GB because both the full-text and vector indexes carry every historical revision. That is
inherent to keeping history rather than a defect, but it is the reason the flag is opt-in: a
caller who wants a fast import of the current state should not pay for it.

## Open points (still not established)

- Whether `project/` is written by every build or only by some workflows. The migrator imports it
  as notes either way, because real stores contain it.
- The git history is replayed only with `--history`; see Migrator behaviour above for the rules it
  follows.
- Whether legacy blocks carry ids or timestamps inside the Markdown is unknown; such text is kept
  verbatim in `content`.
- Runtime bookkeeping (recall ledgers, sync queues, locks, transcripts, facts queues, reflection
  runs) is deliberately not imported: it describes machinery the MIT store does not have, and it
  is the bulk of the disk usage on real machines.


## Delta (omo v5.1.52, 2026-10-30)

This delta records one change in the legacy format that the MIT importer must honour: files may
declare when they stop being true. It was written from the OmO public documentation of the
`expires` frontmatter key and the shape of accepted values; the spec writer read SUL and upstream
sources for understanding only and reproduces facts, not wording or code. Everything else in the
v5.1.52 range concerns machinery the MIT store does not have (account sync, prompt projection,
workers, write guards) and is listed as N-A in the mapping table.

### 1.1 The `expires` frontmatter key (interface fact)

A legacy memory file may carry `expires: <value>` in its YAML frontmatter. The importer reads it
as an instant in UTC (epoch milliseconds) using these rules:

- Accepted: a calendar date `YYYY-MM-DD` (the instant is 00:00:00.000 UTC of that day); or a
  datetime `YYYY-MM-DDTHH:MM`, optionally followed by `:SS`, which may itself be followed by a
  fraction of 1 to 9 digits; the datetime must end in `Z` or a numeric offset `+HH:MM` /
  `-HH:MM` (offset hour 00-23, minute 00-59). The instant is normalised to UTC. Fractions finer
  than a millisecond are truncated.
- The scalar may be bare, single-quoted or double-quoted; surrounding whitespace is ignored.
- Every other value is treated as if the key were absent. This includes: a date that is not on
  the calendar (for example 31 April), a clock field out of range (hour > 23, minute or second
  > 59), a datetime with no zone designator (its meaning would depend on the importing
  machine's time zone), a separator other than `T` between date and time, prose, an empty
  value, and lists or mappings. Such a value only removes the expiry; the file's content is
  imported as usual and the import does not fail.
- A file with no `expires` key has no expiry. Other undocumented keys remain preserved as
  metadata and uninterpreted, as before.

Given/when/then:

- Given `expires: 2031-03-14`, when parsed, then the instant is 2031-03-14T00:00:00.000Z.
- Given `expires: 2031-03-14T01:20-05:30`, then the instant is 2031-03-14T06:50:00.000Z.
- Given `expires: '2031-03-14T06:50:09.1234567Z'`, then the instant is 2031-03-14T06:50:09.123Z.
- Given `expires: 2031-03-14T06:50:09`, `expires: 2031-04-31`, `expires: 2031-03-14T06:61Z`,
  `expires: 2031-03-14/06:50Z`, `expires: when the release ships`, `expires: [2031-03-14]` or
  `expires:` with nothing after it, then there is no expiry.

### 1.2 The key never rejects a file

The importer interprets `expires` (it is no longer one of the uninterpreted keys) and still
keeps it in the preserved frontmatter. A value that 1.1 cannot read adds exactly one warning for
that file to the migration report (path, plus a reason of the implementer's wording that names
the `expires` value as unusable); the file's blocks are all imported, without an expiry.

Given a file whose `expires` is free text, when imported, then its blocks are imported with no
expiry and the report lists one warning for that path.

### 1.3 Mapping into `valid_until`

For every block of a file with a readable expiry, the imported `working_memory` row sets
`valid_until` to that instant formatted as `YYYY-MM-DDTHH:MM:SS.mmmZ` (UTC, the same format the
store stamps its own expiries with, so string comparison against "now" stays correct). The
block's `metadata_json` gains `"expires": <the original value as written>`.

- The bound is exclusive: the store counts a row as live only while `valid_until` is later
  than now, so when now equals the expiry instant the row is already out of recall and of the
  active counts. No new rule is needed for this.
- Files already expired at import time are still imported (history is kept; recall and active
  counts exclude them) and are counted (per file) in a new report field `expired_on_import`, in text and
  JSON output.
- Row ids do not depend on `expires` (the id recipe is unchanged), so re-running the import
  after a user edits only `expires` skips the existing rows and does not change their
  `valid_until`. This is reported through `skipped_existing` as before.
- A file with no readable expiry leaves `valid_until` null, as before.
- With `--dry-run`, `expired_on_import` is computed against the current time and nothing is
  written.

Given/when/then:

- Given `notes/trip.md` with `expires: 2031-03-14` and three list items, when imported at
  2031-03-10T12:00Z, then three rows exist, each with `valid_until` 2031-03-14T00:00:00.000Z;
  recall at 2031-03-13T23:59:59.999Z returns them and recall at 2031-03-14T00:00:00.000Z does not.
- Given the same file imported at 2031-04-02T00:00Z, then three rows exist, none is recalled, and
  the report shows `expired_on_import` 1 (it counts files, not blocks).
- Given an import report for a tree with no `expires` keys, then `expired_on_import` is 0.

Error cases: none new; every failure mode of 1.1 degrades to "no expiry".

### Mapping table

| census row | delta § | APPLIES/N-A | reason |
| --- | --- | --- | --- |
| omo-memory-01 | - | N-A | N-A reason: genesis key-bundle validation belongs to the encrypted account-sync stack; maho-memory is a local SQLite store with no accounts, keyrings or wraps |
| omo-memory-02 | - | N-A | N-A reason: genesis preparation (sealed key wraps) has no counterpart: no encrypted sync in maho-memory |
| omo-memory-03 | - | N-A | N-A reason: genesis record building/signing has no counterpart: no signed append log |
| omo-memory-04 | - | N-A | N-A reason: conformance vector for a genesis format the MIT store does not have |
| omo-memory-05 | - | N-A | N-A reason: text is a desktop-app sign-in hint for the account sync; maho-memory has no sign-in and no sync failure messages |
| omo-memory-06 | - | N-A | N-A reason: agreements ledger is a git-tree file of the legacy store; MIT store keeps no agreements ledger or git reads |
| omo-memory-07 | - | N-A | N-A reason: agreements notice watermark: MIT store has no notice mechanism |
| omo-memory-08 | - | N-A | N-A reason: watermark file hardening: MIT store writes no watermark files |
| omo-memory-09 | - | N-A | N-A reason: soul notice watermark: no counterpart |
| omo-memory-10 | - | N-A | N-A reason: soul watermark hardening: no counterpart |
| omo-memory-11 | - | N-A | N-A reason: persisted compiled-block cache; MIT store has no compiled block or block cache (recall queries SQLite directly) |
| omo-memory-12 | 1.1 | APPLIES | `expires` frontmatter is a documented interface key; importer must read it into a UTC instant |
| omo-memory-13 | 1.3 | APPLIES | legacy rule: expired files stop being shown as of the pin instant; MIT analogue is that imported expired rows stay out of recall (valid_until) and the report counts them |
| omo-memory-14 | - | N-A | N-A reason: compiled-block cache and compiler version; no counterpart |
| omo-memory-15 | - | N-A | N-A reason: reading git HEAD from files; MIT store never runs or reads git |
| omo-memory-16 | - | N-A | N-A reason: external-path projection grouping; MIT store has no projection into prompts |
| omo-memory-17 | - | N-A | N-A reason: external-path name folding for projection; no projection |
| omo-memory-19 | 1.2 | APPLIES | `expires` is now an interpreted frontmatter key; a value 1.1 cannot read leaves the file without expiry and is imported anyway |
| omo-memory-20 | - | N-A | N-A reason: persona prompts of the legacy reflection/dream workers; MIT store ships no personas or reflection workers |
| omo-memory-21 | - | N-A | N-A reason: in-process journal lock cancellation (already present upstream); MIT store has no journal lock |
| omo-memory-22 | - | N-A | N-A reason: journal store cancellation; no journal |
| omo-memory-23 | - | N-A | N-A reason: parked fact-queue recovery; MIT store has no fact queue |
| omo-memory-25 | - | N-A | N-A reason: memory patch tool path accounting; MIT tools are the mnemopi_* MCP tools, not a patch tool |
| omo-memory-26 | - | N-A | N-A reason: private desktop-sync wire protocol; no desktop sync |
| omo-memory-27 | - | N-A | N-A reason: desktop-sync channel attach; no desktop sync |
| omo-memory-28 | - | N-A | N-A reason: keychain-pending deferral of account sync; no account or credential store |
| omo-memory-29 | - | N-A | N-A reason: deferred sync retry schedule; no sync runner |
| omo-memory-32 | - | N-A | N-A reason: project memory grant path walk is a write-guard of the legacy memory directory; MIT store exposes no file-path grants |
| omo-memory-33 | - | N-A | N-A reason: write-guard target resolution for legacy memory files; no guard in MIT store |
| omo-memory-34 | - | N-A | N-A reason: expiry notice in the pinned prompt block; MIT store has no pinned projection (expired rows are simply not recalled, see 1.3) |
| omo-memory-35 | - | N-A | N-A reason: pin record bookkeeping for the legacy prompt projection; no pin |
| omo-memory-36 | - | N-A | N-A reason: prompt assembly for the legacy memory block; MIT store has no prompt notice |
| omo-memory-37 | - | N-A | N-A reason: tool metadata line for the legacy file tools; the MIT tools already document an optional expiry argument (valid_until) and the legacy `expires` key is covered by 1.1 |
| omo-memory-39 | - | N-A | N-A reason: identity reconcile single-flight; no identity runtime |
| omo-memory-40 | - | N-A | N-A reason: per-session user-activity reconcile dedup; no such runtime |
| omo-memory-41 | - | N-A | N-A reason: facts wiring abort handling (already present upstream); no facts queue |
| omo-memory-42 | - | N-A | N-A reason: journal wiring and RPC bridge abort guards; no journal or bridge |
| omo-memory-43 | - | N-A | N-A reason: reflection trigger abort threading; no reflection triggers |
| omo-memory-44 | - | N-A | N-A reason: settle-time wiring observer; no settle wiring |
| omo-memory-45 | - | N-A | N-A reason: warn sink for notice watermarks; no watermarks |
| omo-memory-46 | - | N-A | N-A reason: worker run reconciliation abortability; MIT store has no background workers or run sentinels |
| omo-memory-47 | - | N-A | N-A reason: Windows tree kill for the legacy memory worker supervisor; no worker supervisor |

Rows omo-memory-24, -30, -31, -38, -48, -49 and -50 are not routed to `crates/maho-memory`
(N-A upstream: test fixtures, hook labels, docs, JS packaging; or targeted at `maho-core`) and
are outside this table.

Leak self-check:

- [x] no fenced code block is Rust, TypeScript or JS from the SUL tree (this delta has none);
- [x] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt (the
      rules in 1.1 are stated as behaviour in the writer's own words);
- [x] no list or table mirrors a SUL constant table row-for-row (the only table is the mapping
      table; the key `expires` and the date forms are interface facts);
- [x] no proposed identifier is an OmO-original name (`expired_on_import` is neutral);
- [x] OmO-original features are absent (no persona, no projection, no pin or notice state).
