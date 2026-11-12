# Unwired modules in `maho-memory`

`maho-memory` declares 85 modules. **21 are reachable** from a real entry point and **64 are not**.
Those 64 hold 24,798 lines and about 1,106 unit tests that no user can reach. (Counts below include
`lib.rs` itself, which is why they read 86 and 22.)

The entry points are `extension`, `store`, `recall`, `legacy`, `mcp`, `kibitzer`, `reflection`
**and the paths `maho-cli` itself reaches into the crate** — `banks`, `retention`, and
`session_provider`/`transcript_search`, which back the shipped `mhc memory search-sessions` command.
An earlier revision of this file seeded only from inside the crate and consequently listed those last
two as unwired; they are not, and counting them is the difference between 66 orphans and 64.

This file exists so that unwired surface stops reading as progress. A module is listed here because
it is *deliberately* not wired yet, with the reason. Nothing here is a completed feature.

Reachability was measured by parsing `use crate::…` trees (groups expanded, nested lists included)
**and** fully-qualified expression paths such as `crate::expiry::parse_expiry_instant(…)`, dropping
`#[cfg(test)]` bodies so a module used only by its own tests counts as unreachable, then closing
transitively from the entry points above. Both halves are required: a `use`-only parser reports wired
modules as orphans, and `expiry`, `git_log`, `frontmatter`, `normalize`, `retention` and
`time_format` were all false orphans under that bug before it was fixed. Re-running that measurement
is the way to check this file has not gone stale.

## The deletion boundary is compiler-proven, not inferred

The orphan set above was derived by parsing `use` groups and expression paths out of the reachable
modules. That is an argument, not a proof, so it was checked with the compiler.

All 64 orphan modules were deleted from a scratch copy together with their `pub mod` declarations and
`pub use` re-exports. No further edit was needed. The result:

| | modules (incl. `lib.rs`) | lines | tests | maho-memory | maho-cli | clippy |
|---|---|---|---|---|---|---|
| full crate | 86 | 38,093 | 1,474 | ok | ok | clean |
| 64 orphans removed | 22 | 13,160 | 368 | **exit 0** | **exit 0** | **clean** |

Both crates are built, because the lib alone does not know who reaches into it. An earlier proof
stopped at `maho-memory` and used a 66-module list that wrongly included `session_provider` and
`transcript_search`; that version would have broken `mhc memory search-sessions` had the deletion
actually shipped. The 64-module list, built against both crates, is the trustworthy one.

The two line totals differ by 135 more than the 24,798 above: that difference is the `pub mod` and
`pub use` lines stripped out of `lib.rs`, which are counted in the full total but survive in neither.

No reachable module referenced an orphan: the five-line strip was enough to make the reduced crate
compile. The reverse direction is **not** empty — 24 edges run from orphans into kept modules, so the
corpus half depends on `cursor`, `frontmatter`, `normalize`, `redact`, `reflection`,
`reflection_machine`, `store`, `time_format` and `transcript_search`. Deletion is safe because the
dependency is one-way; splitting the crate apart would
need those nine carried across with it. The kept twenty-two are `banks`, `cursor`, `expiry`, `extension`, `frontmatter`,
`git_log`, `kibitzer`, `legacy`, `lib`, `mcp`, `normalize`, `recall`, `redact`, `reflection`,
`reflection_machine`, `retention`, `schema`, `session_provider`, `store`, `text`, `time_format`,
`transcript_search`.

Two things this measurement does **not** say. It does not say the orphans are worthless — several are
complete implementations of features this profile does not run. And it does not decide what happens to
them; `orphan_sweep` was wired to a producer and a caller as an experiment and both were absent, so
the module was left unwired rather than shipped as a 67th orphan.

Reproduce it: copy `crates/maho-memory/src` aside, delete the modules listed here, strip the matching
`pub mod`/`pub use` lines from `lib.rs`, then build **both** `maho-memory` and `maho-cli` and run
`cargo test -p maho-memory --lib`. Building only the lib is what let an earlier revision of this file
call `session_provider` and `transcript_search` unwired when `mhc memory search-sessions` runs them.

## Why these exist

Phase 1b ported `memory-core` module-for-module. The port is faithful, and faithfulness alone does
not make something reachable: the upstream subsystems are driven by call sites that live in the
agent runtime, in git automation, or in Tokio/fs plumbing that a synchronous store does not have.
Porting the module and wiring it are two different jobs, and only the first was scheduled.

## Clusters, largest first

Clusters are modules that reference each other; they will be wired or removed together, so the
cluster is the unit of decision, not the file.

| tests | modules | what it is | why not wired |
|---:|---|---|---|
| 157 | `bm25`, `bm25_store`, `english_stem`, `ledger`, `project_corpus`, `recall_strategy`, `recall_wire`, `select` | a second retrieval engine (BM25 + strategy selection) beside the live `recall` | `recall` already ranks with `text` + FTS. Two rankers serving one query is the design question, not a wiring task: `select` and `recall_strategy` are alternatives to `recall`, not helpers for it. |
| 135 | `facts_anchor`, `facts_parked`, `facts_queue`, `facts_queue_scan`, `facts_schema`, `failure_backoff`, `failure_selection`, `payload_cap` | the facts-extraction queue: batches, claims, watermarks, failure ledger | the producer is the agent's fact-extraction turn, which the MIT profile does not run. Queue without a producer writes nothing. |
| 121 | `consent`, `fact_display`, `facts_extraction`, `people_store`, `prompt`, `records` | fact/people record parsing, consent gate, prompt rendering | same missing producer as above; `consent` additionally needs a UI decision surface. |
| 122 | `host_identity`, `lock_acquire`, `lock_record`, `locks`, `journal`, `random_ids`, `reflection_reservation`, `stale_holder` | cross-process identity, lock/reservation state, and the transcript journal | the live store is single-process and SQLite already serializes writers. These protect a multi-process markdown store that does not exist here yet, and the journal has no reader without a reflection runner. |
| 69 | `audit`, `broken_links`, `paths` | corpus audit: 6 finding codes, silent-corruption detection | it audits a markdown corpus. `--history` and the importer are the only corpus readers, and neither runs audit. |
| 36 | `recall_gate`, `recall_render` | nudge gate (instructions blocked, facts allowed) and recalled-memory rendering | belongs to the agent prompt path (`maho-omo-memory`), not the store. |
| 35 | `facts_person_routing`, `person_index` | routing facts to people records, person index | depends on embedding producers (another provider model). |
| 114 | `reflection_completion`, `completion_repair`, `description_preflight`, `orphan_sweep`, `receipts`, `secret_scan_records` | finishing a reflection run: validating its output, repairing dropped entries, scanning for secrets, reclaiming what a killed run left behind | none of it can run because `reflection.rs` never creates a worktree or branch, so there is never a completed run to validate or a leftover to reclaim. The decisions themselves are pure logic; the producer is what is missing. |
| 56 | `agreements`, `persona_assets`, `soul_paths`, `prompt_render` | projecting memory into system-prompt slots (agreements, persona, soul) | `maho-omo-memory` owns prompt assembly in this repo. Duplicating the slot projector here would create two answers to one question. |
| 51 | `recall_planner`, `reminders`, `seeds` | query planning and seeded recall before an agent turn | belongs to the prompt path, and `recall` is already called directly by the CLI, the agent tool and MCP. |
| 73 | `facts_abort`, `facts_batch_range`, `facts_commit`, `facts_failures_schema`, `facts_note` | the write half of the facts queue | same missing producer as the queue above. |
| 79 | `git_tree`, `git_status`, `git_layout`, `git_encoding` | reading trees, status and layout from a git-backed corpus | the importer reads history and paths but never a tree or status; these are helpers for a git-backed *writer* the MIT profile does not have. |
| 55 | `errno_code`, `fs_resilience`, `lookalike_keys`, `frontmatter_entries` | errno classification, retrying filesystem calls, near-duplicate key detection | defensive plumbing for a multi-process markdown store, and the importer's own frontmatter reader covers what it needs. |
| 11 | `identity_layout` | layout of an identity's on-disk repo | the importer walks the real layout directly and does not need a second model of it. |

Every one of the 64 unwired modules appears in a row above. Test counts are measured per module from
its `#[cfg(test)]` block, not estimated.

## What is *not* a deferral

Wiring done, kept:

- `git_log` — `legacy.rs` calls `crate::git_log::LOG_FORMAT` for its `--history` reads, so this module
  has a real caller.
- `expiry` — implements the `expires` contract; `legacy::parse_expires` now delegates to it rather
  than keeping a second copy.
- `time_format` — renders instants for the importer too; `legacy::format_instant` delegates.

`git_tree` and `git_status` are **not** in that list, despite an earlier revision of this file saying
so. A `pub mod` plus a `pub use` in `lib.rs` is a re-export, not a caller; the only reference either
has is `git_encoding`, which is itself unwired. They are listed as orphans above.

The pattern those three followed is the one worth repeating: an orphan is worth keeping only when a
reachable module can use it *instead of* duplicating it. `frontmatter` was checked the same way and
left alone: its `split_frontmatter` returns raw header text for a single block, while the importer
needs YAML parsing across stacked blocks, so they are different contracts rather than duplicates.

## Open decision

Whether this crate keeps two architectures at all is unresolved. The options are: delete the
unwired half, keep both with separated roles, or move the corpus half into its own crate.

**Where it stands (2026-11-12).** Keep both, with the boundary documented and proven — that is the
current state, and it is now a decision rather than an accident, for three reasons:

1. Deletion is *possible* (the compiler proof above) but not *desirable yet*. The user's standing
   requirement is to include every feature, and the producers these modules need are on the
   Phase 1-8 track, not abandoned. Deleting 24,798 lines to make a number look better would have to
   be undone.
2. A crate split would not help today. The corpus half has 23 one-way dependencies into
   `store`/`redact`/`frontmatter`/`text`/`normalize`/`cursor`/`time_format`, so a
   `maho-memory-corpus` crate would either duplicate them or drag them along — and a crate that
   nothing depends on hides dead code instead of revealing it.
3. The real problem was never the file layout. It was that unwired modules read as progress. The
   cluster table and the compiler proof fix that directly.

So the fork becomes actionable only when one of two things happens: the reflection runner and the
facts-extraction producer get scheduled (then wire these modules), or that track is abandoned (then
delete them — the proof above shows nothing else breaks). Until then, nothing here should be deleted,
and nothing here should be counted as shipped.
