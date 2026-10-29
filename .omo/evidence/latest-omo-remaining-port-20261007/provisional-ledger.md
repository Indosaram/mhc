# Provisional Port Ledger Summary

**Status**: Provisional
**Interfaces Status**: `pending_correction` (interfaces completion independently rejected; under correction)
**Generated**: 2026-10-08T00:11:09.086Z

## Executive Summary & Blockers

This provisional parity ledger synthesizes source audit findings across four contract domains:
1. **Runtime** (7 units): Finalized.
2. **Shared-Core** (111 rows): Finalized.
3. **Memory-Config** (504 records): Finalized.
4. **Interfaces** (879 groups / 1,222 expanded rows): **Held in `pending_correction`**.

### Blockers to Final Ledger
* **Interfaces Report Correction Pending**: Interfaces completion was independently rejected and the report is under active correction.
* **Unproven Missing Claims Held**: The preliminary 79 missing rows in the interfaces report lack concrete consumer absence or source-difference proof and are held as `pending_unclassified`. They are strictly excluded from candidate implementation lanes.

---

## Counters

| Counter | Count | Description |
| --- | ---: | --- |
| **total_finalized_rows** | **622** | Finalized rows across Runtime (7), Shared-Core (111), Memory-Config (504) |
| **interfaces_expanded_rows** | **1,222** | Interfaces contract rows (879 groups), held as `pending_correction` |
| **unclassified** | **0** | Zero unclassified rows among finalized domain populations |
| **implementation_required** | **0** | Zero implementation gaps across all finalized domains |
| **verification_required** | **257** | Current source paths exist; executable test/parity proof required |
| **closed_complete** | **1** | Bounded contract fact verified complete (RU-01) |
| **closed_scope** | **364** | Explicit exclusions and owner-deferred rows citing controlling decisions |
| **exclusions_without_decision** | **0** | Every exclusion/deferred row cites an authoritative decision |

---

## Domain Breakdown

| Domain | Total Rows/Units | Implementation Required | Verification Required | Closed Complete | Closed Scope | Status |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| **Runtime** | 7 units | 0 | 6 | 1 | 0 | Finalized |
| **Shared-Core** | 111 rows | 0 | 102 | 0 | 9 | Finalized |
| **Memory-Config** | 504 records | 0 | 149 | 0 | 355 | Finalized |
| **Interfaces** | 1,222 rows (879 groups) | *Pending* | *Pending* | *Pending* | *Pending* | **Pending Correction** |

---

## Candidate Implementation Lanes (Dependency-Ordered)

* **Candidate Lanes**: **None** (zero rows meet `implementation_required` criteria across finalized domains).
* Current main implements `RU-03a` in `crates/maho-cli/src/main.rs:34-40` with `maho_rpc::supervisor_route::dispatch_internal_supervisor(&argv)` and test coverage in `crates/maho-cli/tests/supervisor_route.rs`. It is classified as `verification_required` for end-to-end parity execution.

---

## Verification Matrix (Grouped by Current Crate / Consumer)

For `verification_required` rows (total: **257**), grouped by target crate:

| Target Crate / Consumer | Verification Required Rows | Primary Scope |
| --- | ---: | --- |
| `crates/omo/components/maho-omo-memory` | 59 | Reflection settings, memory wiring & schemas |
| `crates/omo/omo-config-core` | 48 | Config schema options & settings |
| `crates/omo/memory-core` | 32 | Memory models, stores & vector indexing |
| `crates/builtins/maho-ext-mcp` | 17 | MCP client & stdio tool bridges |
| `crates/omo/components` | 13 | Component composition & lifecycle hooks |
| `crates/omo/comment-checker-core` | 9 | Comment validation rules & parser |
| `crates/omo/lsp-core` | 9 | LSP client & protocol adapters |
| `crates/omo/model-core` | 9 | Model catalog & provider profiles |
| `crates/builtins/maho-ext-rules` | 8 | Rules engine matcher & pattern predicates |
| `crates/omo/lsp-daemon` | 8 | Daemon IPC & session bindings |
| `crates/builtins/maho-ext-prompt-preset` | 6 | Prompt preset templates & registry |
| `crates/builtins/maho-ext-nested-agents-md` | 6 | Markdown nested agent loader |
| `crates/omo/telemetry-core` | 6 | Telemetry metrics & traces |
| `crates/omo/utils` | 6 | Runtime utilities & formatting |
| `crates/omo/ast-grep-mcp` | 5 | AST-grep MCP tool server |
| `crates/omo/mcp-stdio-core` | 4 | MCP stdio framing & pipe handling |
| `crates/maho-cli` | 3 | RU-03a supervisor dispatch, RU-03b multi-session entry & runtime factory |
| `crates/omo/config-migration` | 2 | Schema migration & config transforms |
| `crates/omo/maho-omo` | 2 | Maho-omo integration hooks |
| `crates/omo/boulder-state` | 1 | RU-02 stale-work storage & timestamp parser |
| `crates/maho-rpc` | 1 | RU-03c host watchdog arming & lifetime binding |
| `crates/omo/components/maho-omo-task` | 1 | RU-04 DAG production registration |
| `crates/omo/senpi-task` | 1 | RU-05 team query tools (team_status / team_list) |
| `crates/omo/git-bash-mcp` | 1 | Git bash tool bridge |

---

## Exclusions & Deferred Decisions

Every row classified as `closed_scope` (364 rows) cites an authoritative controlling decision:
* **`scope-opencode-excluded`**: 4 rows in Shared-Core (hashline OpenCode edit engine) + 353 contract alias/scope rows in Memory-Config.
* **`dec-sc-claude-code-deferred`**: 5 rows in Shared-Core (Claude Code agent/command/plugin/MCP loaders deferred by owner).
* **`DEC-MC-B70`**: 1 row in Memory-Config (Published schema generation asset policy).
* **`DEC-C-A3`**: 1 row in Memory-Config (Agent-dir setup/import five-entry copy policy).
* **`exclusions_without_decision`**: **0**.
