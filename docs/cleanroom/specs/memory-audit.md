# Corpus audit — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/audit/audit.rs`. Implemented in
`crates/maho-memory/src/audit.rs`.

The audit answers one question: **is anything in this corpus broken in a way the reader would not notice?**
Every finding here is silent in normal use — the note still renders, the block still fits, the link still looks
like a link. That is why they need a dedicated pass.

## Shape

`audit_files(&[(path, bytes)], &MemoryAuditOptions) -> MemoryAuditReport`

The corpus arrives as a list rather than a directory walk. The rules are then testable without a filesystem, and
the caller keeps ownership of symlink policy — following a link out of the repository is the caller's decision,
not an audit rule.

`MemoryAuditReport` carries `version: 1`, the issue list, and a count for **every** code including zeros. A
missing key would be ambiguous between "no findings" and "not checked".

Files are sorted before processing, so the same corpus always produces the same report.

## Codes

| Code | Meaning |
|---|---|
| `link_dangling` | a wiki or inline link resolves to nothing |
| `frontmatter_invalid` | a content path whose header breaks the grammar |
| `content_duplicate` | two or more files share a body |
| `path_orphan` | a `.md` file outside the known homes |
| `file_unreadable` | bytes are not valid UTF-8 |
| `system_pressure` | the system prompt has reached 80% of its budget |

## Rules and their reasoning

**Homes.** A note lives under `system`, `reference`, `notes`, `people`, or `skills`. `memory/` may prefix the
path. `ARCHIVE.md` and `README.md` are allowed at the root. Anything else is an orphan: a file the projection
will never read, which is the same as not existing while still costing disk and confusing search.

An orphan without frontmatter gets a longer message, because the two facts together usually mean a file was
dropped in by hand rather than written through the tool.

**Non-markdown files are not audited at all.** They are not memory content, and flagging them would train the
reader to ignore the report.

**Duplicates compare bodies, not files.** Frontmatter differs by design (each block needs its own description),
so hashing the whole file would never fire. Empty bodies are excluded — many skeleton files are legitimately
empty and would otherwise all collide.

Only the *later* paths are reported, each pointing at the first. Three copies produce two findings, not three:
the first file is the one to keep.

**Dangling links** reuse `broken_links::dangling_links`, which already skips fenced blocks, inline code, images,
anchors, and absolute URLs. An escaping link is reported separately, with `escapes repository` in the detail —
the distinction matters, because a link that leaves the repository is a privacy question rather than a typo.

**System pressure** fires at 80% of the budget. At four-fifths, the next addition is likely to push the prompt
over, and the person can decide what to drop instead of discovering the overflow when a block silently stops
fitting. Both numbers must be present, and a zero budget is ignored rather than dividing by zero.

## Failure direction

Every ambiguity resolves toward **reporting**. Unreadable bytes still get their orphan finding. An orphan with
broken frontmatter gets both. A missing token count suppresses only the pressure finding, never the others.

The one thing the audit never does is modify anything. It produces findings; acting on them is a separate,
explicit step.
