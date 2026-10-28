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
| Config keys that shape the store (`memory.enabled`, `memory.people.*`, `memory.projection.max_entries_per_directory`=40, `memory.projection.max_bytes`=24576, `memory.sync.remote`) | `docs/cleanroom/specs/config-keys.md` § `memory` |

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
