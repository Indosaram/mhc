# Legacy memory store — on-disk format (spec for the MIT migrator)

Phase 2, step 3 of the MIT migration. This document describes the persistent memory data that
earlier mhc builds (the `omo` feature, "memory-core") leave on a user's disk, so that the MIT
memory crate (`crates/maho-memory`) can import it into its SQLite store. It is the input for the
migrator only; the new store has its own format (mnemopi's SQLite schema, see
`crates/maho-memory/PROVENANCE.md`).

## Sources this spec was written from

The spec writer did not open any SUL source (`crates/omo/**`, the omo-gated `maho-cli` modules,
OmO/COMP checkouts). No legacy memory directory exists in the sandbox the work was done in (no
`~/.omo`, no `MAHO_MEMORY_HOME`), so nothing was observed from real data. Everything below comes
from the repository's user-facing documents and MIT call sites:

| Fact | Where it is stated |
| --- | --- |
| Root directory is `~/.omo/memory`, overridable with `MAHO_MEMORY_HOME` | `docs/configuration.md` (environment table) |
| The store is a "Git-backed persistent memory filesystem, identity profiles"; "Markdown block store, git auto-commit" | `AGENTS.md` (structure and WHERE TO LOOK tables) |
| Memory is plain files the user owns and may edit by hand; every change is a git commit whose history can be inspected and undone | `crates/maho-interactive/src/tips/catalog/memory_tips.rs` (`memory.memfs-command`, `memory.repository-command`, `memory.recompile-command`), a port of senpi's MIT `memory-tips.ts` |
| The primary human is described by the card `system/human.md`: its display name is the card's description; YAML frontmatter may carry `aliases`; the seeded name is `Human` | `docs/divergences.md` SD-7 |
| Facts about the primary human and about the project are written to `notes/facts/<month>.md` | `docs/divergences.md` SD-7 |
| Third-party people profiles live under `people/<slug>/`; they are written only when `memory.people.enabled` is true (default false) | `docs/divergences.md` SD-7, `README.md`, `README.en.md` |
| Config keys that shape the store (`memory.enabled`, `memory.people.*`, `memory.projection.max_entries_per_directory`=0, `memory.projection.max_bytes`=40960 (omo v5.1.52 defaults; earlier builds used 40 and 24576), `memory.sync.remote`) | `docs/cleanroom/specs/config-keys.md` § `memory` and § Delta (omo v5.1.52) D1 |

## Directory layout

```
<memory home>/                 MAHO_MEMORY_HOME, else ~/.omo/memory
  .git/                        git repository (auto-commit); may have a remote (memory.sync.remote)
  system/
    human.md                   card for the primary human (identity)
    *.md                       other system cards (names not documented)
  notes/
    facts/
      <month>.md               extracted facts, one file per month
    *.md / */*.md              other notes (not documented further)
  people/
    <slug>/                    one directory per third-party person (only with people.enabled)
      *.md
  ...                          any other directories of Markdown files the user or agent created
```

- `<month>` is a calendar month. The exact spelling is not documented; the migrator must accept
  any file name and only use a name of the form `YYYY-MM` (optionally followed by more text) as a
  date hint.
- The tree is the user's own: files may be added, renamed or hand-edited. The migrator must not
  assume a fixed set of files and must tolerate empty, oversized or malformed files.
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
  line that is exactly `---`. A file whose frontmatter does not close, or does not parse as a YAML
  mapping, is read as body-only (the whole text is body) and reported as a warning.
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
| `importance` | `0.9` for `system/` files, `0.7` for `notes/facts/`, `0.6` otherwise |
| `memory_type` | `identity` for `system/human.md`, `fact` for `notes/facts/`, `person` for `people/`, `note` otherwise |
| `timestamp` | the month hint (`YYYY-MM-01T00:00:00.000Z`) when the file name starts with `YYYY-MM`, else the file's modification time, else the import time |
| `metadata_json` | `{"legacy_path": <relative path>, "section": <heading or null>, "block": <index>, "frontmatter": <mapping or null>, "importer": "maho-memory legacy-v1"}` |
| `consolidated_at` | the import time (durable; never trimmed as scratch) |

People profiles: files under `people/` are imported only when the caller opts in
(`--include-people`, or `memory.people.enabled: true` in the merged omo config). Otherwise they
are counted as skipped. This keeps the privacy default of SD-7: data about third parties is not
copied into a new place unless the user enabled people memory.

## Migrator behaviour

- Input: the memory home (explicit `--from`, else `MAHO_MEMORY_HOME`, else `~/.omo/memory`).
  A missing directory is not an error: the report says there is nothing to import.
- Read-only on the legacy tree. It never writes, moves, commits or deletes legacy files, and it
  does not run git.
- Idempotent: rows whose id already exists are skipped and counted as `skipped_existing`.
- `--dry-run` parses and reports without writing.
- Report (text and JSON): files scanned, files imported, blocks imported, skipped existing,
  skipped people files, skipped non-Markdown files, warnings (path + reason).
- Symbolic links are not followed (a link is reported and skipped).
- Files larger than 4 MiB are skipped with a warning.

## Open points (not established from the sources above)

- The exact month file name, the names and meaning of other `system/` cards, and any per-project
  sub-tree are undocumented. The migrator treats them generically.
- The git history (older versions of a block) is not imported; only the working tree is.
- Whether legacy blocks carry ids or timestamps inside the Markdown is unknown; such text is kept
  verbatim in `content`.
- Recall ledgers, sync queues, locks or other bookkeeping files the legacy store may keep are not
  documented; anything that is not a `.md` file is skipped.


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
