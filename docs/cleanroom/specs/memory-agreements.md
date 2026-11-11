# Memory agreements and projection — behaviour spec (MIT clean room)

Phase 1, item 7, first slice of the memory clean room. This document describes the **behaviour** the
legacy `memory-core` (SUL) provides for standing agreements and for projecting memory into the
system prompt, so that `crates/maho-memory` can implement it from the contract instead of the code.
It is written from observed data and from the runtime's own user-visible output, not from SUL
source text.

Companion specs: `memory-format.md` (on-disk store layout, the importer's input).

## Sources this spec was written from

| Fact | Where it is stated / observed |
| --- | --- |
| Agreement blocks live at `<repo>/system/agreements.md` | live store `~/.omo/memory/agents/*/repo/system/agreements.md` |
| Block frontmatter is `description:` only | observed head of every `agreements.md` |
| 64 entries with `by: agent`, 43 `status: active`, 21 `status: needs…` | counted across all 269 identity repos |
| 86 `agreement-event` comment lines | counted the same way |
| Projection rules (which blocks reach the prompt, `expires`, `read_only`) | the runtime's own injected memory preamble, observable in any session |

## 1. Agreement entry

One agreement is one list item in `system/agreements.md`:

```
- [<scope>] <text> <!-- id: <hex12>; by: <author>; n: <count>; since: <date>; last: <date>; status: <status> -->
```

| Field | Shape | Meaning |
| --- | --- | --- |
| `scope` | `global` \| `project` | applies to every project, or only this repo |
| `text` | free text, one line | the agreement itself, in the person's words |
| `id` | 12 lowercase hex | stable id; new entries get a fresh one, existing ids never change |
| `by` | `agent` (observed) | who recorded it; treat as an open string, do not enumerate |
| `n` | integer ≥ 1 | how many times the agreement has been **broken** |
| `since` | `YYYY-MM-DD` | first recording |
| `last` | `YYYY-MM-DD` | most recent break (not most recent edit) |
| `status` | `active` \| `needs-human` | `needs-human` once `n` crosses the threshold (observed at `n ≥ 3` in every promotion, `n = 2` left `active`) |

Rules:

- The metadata comment is the only machine-readable part. A reader must not infer `n` or `status`
  from the prose.
- An entry keeps its line position; appending a new agreement is an addition at the end of its
  scope section, never a rewrite of the file.
- `n` increments only on an observed break. Editing the text (clarifying wording) does not change
  `n`, `since` or `last`.
- Promotion to `needs-human` is one-way within a session's lifetime: the agent must not continue
  past a breached agreement without asking. A human can reset it; the agent cannot.

## 2. Agreement events

Every break appends one line immediately after its entry:

```
<!-- agreement-event: <hex64> -->
```

- 86 events observed against 64 entries, so one entry can have many.
- The value is a 64-hex digest, unique per event; the spec does not require a particular preimage,
  only that equal events dedupe and different events differ.
- Events are append-only history. Never delete an event to decrement `n`; `n` counts events.

## 3. Projection into the system prompt

The memory store is a git-backed Markdown tree. Only committed blocks are projected, and projection
happens at the start of a run, so a change made mid-run affects the next run.

### Which paths project and how

| Path | Projection |
| --- | --- |
| `system/persona.md` | verbatim body (frontmatter stripped) |
| `system/identity.md` | verbatim, optional; never seeded |
| `system/human.md` | verbatim in the person slot |
| `system/boundaries.md` | verbatim, in the boundaries slot |
| `system/self-aware.md` | verbatim, in the self-aware slot |
| any other `system/*.md` | wrapped as a nested section named after the file, body verbatim |
| everything outside `system/` (`notes/`, `reference/`, `people/`, `project/`, `skills/`) | **names only**, listed in an `external_projection` index; bodies are never injected |

### Frontmatter keys that change projection

| Key | Effect |
| --- | --- |
| `description` | required; shown in the `external_projection` listing and in the memory index |
| `expires` | UTC date or ISO instant; the block stops projecting after that instant with no rewrite needed |
| `read_only: "true"` | the block projects but rejects modification |

- Frontmatter is stripped from the projected body.
- A block with no `description` is not projected; it is a malformed block, not a silent default.
- `expires` is evaluated against the run's start time, not against each read.

## 4. What the MIT memory crate must provide

Minimum behaviour, in order of user impact:

1. **Parse** `system/agreements.md` into entries: scope, text, id, by, n, since, last, status.
   Unknown metadata keys and unknown `status` values are preserved, not rejected.
2. **Count** `agreement-event` lines per entry so `n` can be cross-checked against history; a
   mismatch is reported, never silently repaired.
3. **Project** the `system/` blocks into the prompt using the table above, honouring `expires` and
   `read_only`, and emitting the `external_projection` name index for everything else.
4. **Round-trip**: rewriting an untouched file must produce byte-identical output, so a memory
   commit diff shows only real changes.

Out of scope for this slice: recall ranking (already implemented in `recall.rs`), facts buckets,
reflection/dream runs, kibitzer. Those are separate specs.

## 5. Divergence notes

- The live stores disagree about which top-level directories are "memory content":
  `memfs/paths.rs::is_memory_content_path` allows `system | reference | people` plus
  `skills/<slug>/SKILL.md`, while `audit/audit.rs` also allows `notes`. **Neither lists `project/`**,
  yet 193 live files sit there. The importer reads the union; the clean room should state the union
  explicitly rather than inherit the mismatch.
- `by:` values other than `agent` were not observed. Treat the field as open.

## 6. Not ported: agreement mutation

The reference package has `agreements/{types,format,record,mutate}.rs` with typed enums
(`AgreementScope`, `AgreementAuthor`, `AgreementStatus`), a reword/ack/drop implementation, and
`add_human_agreement` / `ack_agreement` / `drop_agreement`. None of it is reproduced in this crate.

That code is not missing from the product: it backs the `/agreements` command in
`crates/omo/components/maho-omo-memory/src/commands/agreements.rs`, which is where the person's own
edits to `system/agreements.md` are applied. That command is part of the omo build and works today.

The MIT profile here has no command surface for agreements at all — nothing constructs an
`AgreementItem`, and nothing consumes `Agreements` outside this module's own tests. Porting the
mutation functions would create a second writer of the same file with no caller, and two writers of
`system/agreements.md` that disagree is worse than one: `by: human` items are exactly the ones the
reference refuses to let automation edit.

`agreements.rs` here stays a **reader and projection**: it parses the file, reports break-count
mismatches, blocks work on `needs-human`, and projects system blocks into the prompt. That is the
whole surface this profile needs.

Revisit when the MIT profile gains an agreements command. At that point the writer should be
designed against the same guard the reference uses (automation may not edit a human item), not
inherited by copy.
