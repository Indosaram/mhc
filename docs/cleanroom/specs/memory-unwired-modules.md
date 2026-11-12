# Unwired modules in `maho-memory`

`maho-memory` declares 85 modules. **19 are reachable** from a real entry point
(`extension`, `store`, `recall`, `legacy`, `mcp`, `kibitzer`, `reflection`) and **66 are not**.
Those 66 hold 26,207 lines and 1,158 unit tests that no user can reach.

This file exists so that unwired surface stops reading as progress. A module is listed here because
it is *deliberately* not wired yet, with the reason. Nothing here is a completed feature.

Reachability was measured by parsing `use crate::…` trees (groups expanded, nested lists included),
dropping `#[cfg(test)]` bodies so a module used only by its own tests counts as unreachable, then
closing transitively from the entry points above. Re-running that measurement is the way to check
this file has not gone stale.

## The deletion boundary is compiler-proven, not inferred

The orphan set above was derived by parsing `use` groups and expression paths out of the reachable
modules. That is an argument, not a proof, so it was checked with the compiler.

All 66 orphan modules were deleted from a scratch copy together with their `pub mod` declarations and
`pub use` re-exports. No further edit was needed. The result:

| | modules (incl. `lib.rs`) | lines | tests | build | clippy |
|---|---|---|---|---|---|
| full crate | 86 | 38,093 | 1,474 | ok | clean |
| orphans removed | 20 | 11,722 | 316 | **exit 0** | **clean** |

The two line totals differ by 164 more than the 26,207 above: that difference is the `pub mod` and
`pub use` lines stripped out of `lib.rs`, which belong to neither count.

No reachable module referenced an orphan: the five-line strip was enough to make the reduced crate
compile. The reverse direction is **not** empty — 23 edges run from orphans into kept modules, so the
corpus half depends on `store`, `redact`, `frontmatter`, `text`, `normalize`, `cursor` and
`time_format`. Deletion is safe because the dependency is one-way; splitting the crate apart would
need those seven carried across with it. The kept twenty are `banks`, `cursor`, `expiry`, `extension`, `frontmatter`,
`git_log`, `kibitzer`, `legacy`, `lib`, `mcp`, `normalize`, `recall`, `redact`, `reflection`,
`reflection_machine`, `retention`, `schema`, `store`, `text`, `time_format`.

Two things this measurement does **not** say. It does not say the orphans are worthless — several are
complete implementations of features this profile does not run. And it does not decide what happens to
them; `orphan_sweep` was wired to a producer and a caller as an experiment and both were absent, so
the module was left unwired rather than shipped as a 67th orphan.

Reproduce it: copy `crates/maho-memory/src` aside, delete the modules listed here, strip the matching
`pub mod`/`pub use` lines from `lib.rs`, then run `cargo test -p maho-memory --lib`.

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
| 187 | `bm25`, `bm25_store`, `english_stem`, `ledger`, `project_corpus`, `recall_strategy`, `recall_wire`, `select`, `session_provider`, `transcript_search` | a second retrieval engine (BM25 + strategy selection + transcript search) beside the live `recall` | `recall` already ranks with `text` + FTS. Two rankers serving one query is the design question, not a wiring task: `select` and `recall_strategy` are alternatives to `recall`, not helpers for it. |
| 135 | `facts_anchor`, `facts_parked`, `facts_queue`, `facts_queue_scan`, `facts_schema`, `failure_backoff`, `failure_selection`, `payload_cap` | the facts-extraction queue: batches, claims, watermarks, failure ledger | the producer is the agent's fact-extraction turn, which the MIT profile does not run. Queue without a producer writes nothing. |
| 121 | `consent`, `fact_display`, `facts_extraction`, `people_store`, `prompt`, `records` | fact/people record parsing, consent gate, prompt rendering | same missing producer as above; `consent` additionally needs a UI decision surface. |
| 103 | `host_identity`, `lock_acquire`, `lock_record`, `random_ids`, `reflection_reservation`, `stale_holder` | cross-process identity and lock/reservation state | the live store is single-process and SQLite already serializes writers. These protect a multi-process markdown store that does not exist here yet. |
| 69 | `audit`, `broken_links`, `paths` | corpus audit: 6 finding codes, silent-corruption detection | it audits a markdown corpus. `--history` and the importer are the only corpus readers, and neither runs audit. |
| 36 | `recall_gate`, `recall_render` | nudge gate (instructions blocked, facts allowed) and recalled-memory rendering | belongs to the agent prompt path (`maho-omo-memory`), not the store. |
| 35 | `facts_person_routing`, `person_index` | routing facts to people records, person index | depends on embedding producers (another provider model). |

The remaining 27 clusters are single modules or pairs: `git_layout`/`git_encoding`/`git_status`/
`git_tree` (needed by git automation no caller performs yet), `locks`, `journal`, `receipts`,
`reminders`, `orphan_sweep`, `fs_resilience`, `errno_code`, `seeds`, `soul_paths`, `persona_assets`,
`identity_layout`, `agreements`, `prompt_render`, `description_preflight`, `secret_scan_records`,
`lookalike_keys`, `frontmatter_entries`, `reflection_completion`, `facts_note`, `facts_batch_range`,
`facts_abort`, `facts_commit`, `completion_repair`, `recall_planner`, `facts_failures_schema`.

## What is *not* a deferral

Wiring done, kept:

- `git_log`, `git_tree`, `git_status` — `git_log` parses the importer's `--history` output, which
  gives the other two their first caller chain.
- `expiry` — implements the `expires` contract; `legacy::parse_expires` now delegates to it rather
  than keeping a second copy.
- `time_format` — renders instants for the importer too; `legacy::format_instant` delegates.

The pattern those three followed is the one worth repeating: an orphan is worth keeping only when a
reachable module can use it *instead of* duplicating it. `frontmatter` was checked the same way and
left alone: its `split_frontmatter` returns raw header text for a single block, while the importer
needs YAML parsing across stacked blocks, so they are different contracts rather than duplicates.

## Open decision

Whether this crate keeps two architectures at all is unresolved. The options are: delete the
unwired half, keep both with separated roles, or move the corpus half into its own crate. Until that
is settled, nothing here should be deleted, and nothing here should be counted as shipped.
