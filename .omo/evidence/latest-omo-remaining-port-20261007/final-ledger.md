# Final Port Ledger Summary

**Status**: Finalized
**Interfaces Status**: Accepted (independently verified: structural and declared metadata-only counts both 760/873; 1,222 unique rows; 0 missing; 0 invalid exclusion decisions)
**Generated**: 2026-10-08T00:17:50.146Z

## Executive Summary

This final parity ledger synthesizes audited parity findings across all four contract domains:
1. **Runtime** (7 units): Finalized (1 closed_complete, 6 verification_required, 0 implementation_required).
2. **Shared-Core** (111 rows): Finalized (102 verification_required, 9 closed_scope, 0 implementation_required).
3. **Memory-Config** (504 records): Finalized (149 verification_required, 355 closed_scope, 0 implementation_required).
4. **Interfaces** (879 groups / 1,222 unique expanded rows): Classified (294 verification_required, 928 closed_scope, 0 missing, 0 implementation_required). Doctor schema is excluded by its pinned OpenCode-only producer.

All blockers from the provisional ledger have been cleared:
* The interfaces report was accepted following independent verification of structural and declared metadata-only counters (both 760 groups / 873 rows) across 1,222 unique rows.
* No missing rows or unproven gaps exist (missing = 0 across all domains).
* Candidate implementation lanes are empty (0 rows meet `implementation_required`).

---

## Counters

| Counter | Count | Description |
| --- | ---: | --- |
| **total_rows** | **1,844** | Combined contract population across all four domains (622 finalized + 1,222 interfaces) |
| **finalized_domains_rows** | **622** | Pre-finalized rows from Runtime (7), Shared-Core (111), and Memory-Config (504) |
| **interfaces_expanded_rows** | **1,222** | Expanded interface rows from 879 groups (1,222 unique row IDs) |
| **interfaces_metadata_only_groups** | **760** | Interfaces metadata-only groups (independently verified structural count) |
| **interfaces_metadata_only_rows** | **873** | Interfaces metadata-only rows (independently verified structural count) |
| **unclassified** | **0** | Zero unclassified rows across entire combined inventory |
| **implementation_required** | **0** | Zero implementation gaps across all domains |
| **verification_required** | **551** | Appropriate behavioral or metadata proof required (257 from finalized domains + 294 from interfaces) |
| **closed_complete** | **1** | Bounded contract fact verified complete (RU-01) |
| **closed_scope** | **1,292** | Explicit exclusions and owner-deferred rows citing controlling decisions (364 + 928) |
| **exclusions_without_decision** | **0** | Every closed_scope row inherits or cites an authoritative decision |

---

## Domain Breakdown

| Domain | Total Rows/Units | Implementation Required | Verification Required | Closed Complete | Closed Scope | Status |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| **Runtime** | 7 units | 0 | 6 | 1 | 0 | Finalized |
| **Shared-Core** | 111 rows | 0 | 102 | 0 | 9 | Finalized |
| **Memory-Config** | 504 records | 0 | 149 | 0 | 355 | Finalized |
| **Interfaces** | 1,222 rows (879 groups) | 0 | 294 | 0 | 928 | Classified; verification open |
| **Total** | **1,844** | **0** | **551** | **1** | **1,292** | **Classified; verification open** |

---

## Candidate Implementation Lanes (Dependency-Ordered)

* **Candidate Lanes**: **None** (zero rows meet `implementation_required` criteria across all domains).

---

## Verification Matrix (Grouped by Current Crate / Consumer)

For `verification_required` rows (total: **551** = 257 pre-finalized + 294 interfaces), grouped by crate / target consumer:

### Native Rust Crates (325 rows across 35 crates)

| Target Crate / Consumer | Verification Required Rows | Primary Scope / Source Domains |
| --- | ---: | --- |
| `crates/omo/components/maho-omo-memory` | 59 | Native Rust implementation consumer |
| `crates/omo/omo-config-core` | 48 | Native Rust implementation consumer |
| `crates/omo/memory-core` | 33 | Native Rust implementation consumer |
| `crates/builtins/maho-ext-mcp` | 18 | Native Rust implementation consumer |
| `crates/omo/lsp-core` | 14 | Native Rust implementation consumer |
| `crates/omo/components/maho-omo-skill-commands` | 5 | Native Rust implementation consumer |
| `crates/omo/components/maho-omo-config-watch` | 2 | Native Rust implementation consumer |
| `crates/omo/components/maho-omo-config-resolution` | 1 | Native Rust implementation consumer |
| `crates/omo/components/maho-omo-config-startup` | 1 | Native Rust implementation consumer |
| `crates/omo/components/maho-omo-bundled-skills` | 4 | Additional shared-core rows; combined with its 5 interfaces rows |
| `crates/omo/comment-checker-core` | 10 | Native Rust implementation consumer |
| `crates/omo/model-core` | 10 | Native Rust implementation consumer |
| `crates/builtins/maho-ext-rules` | 9 | Native Rust implementation consumer |
| `crates/omo/lsp-daemon` | 9 | Native Rust implementation consumer |
| `crates/omo/maho-omo` | 9 | Native Rust implementation consumer |
| `crates/omo/ast-grep-mcp` | 8 | Native Rust implementation consumer |
| `crates/maho-interactive` | 8 | Native Rust implementation consumer |
| `crates/maho-cli` | 7 | Native Rust implementation consumer |
| `crates/omo/utils` | 7 | Native Rust implementation consumer |
| `crates/builtins/maho-ext-prompt-preset` | 7 | Native Rust implementation consumer |
| `crates/omo/telemetry-core` | 7 | Native Rust implementation consumer |
| `crates/builtins/maho-ext-nested-agents-md` | 7 | Native Rust implementation consumer |
| `crates/omo/mcp-stdio-core` | 7 | Native Rust implementation consumer |
| `crates/omo/team-core` | 5 | Native Rust implementation consumer |
| `crates/omo/components/maho-omo-bundled-skills` | 5 | Native Rust implementation consumer |
| `crates/omo/senpi-task` | 4 | Native Rust implementation consumer |
| `crates/omo/git-bash-mcp` | 4 | Native Rust implementation consumer |
| `crates/omo/boulder-state` | 2 | Native Rust implementation consumer |
| `crates/omo/config-migration` | 2 | Native Rust implementation consumer |
| `crates/builtins/maho-ext-ask-user` | 2 | Native Rust implementation consumer |
| `crates/builtins/maho-ext-terminal` | 2 | Native Rust implementation consumer |
| `crates/builtins/maho-ext-websearch` | 2 | Native Rust implementation consumer |
| `crates/maho-rpc` | 1 | Native Rust implementation consumer |
| `crates/omo/components/maho-omo-task` | 1 | Native Rust implementation consumer |
| `crates/omo/delegate-core` | 1 | Native Rust implementation consumer |
| `crates/omo/get-worker` | 1 | Native Rust implementation consumer |
| `crates/omo/isolation-core` | 1 | Native Rust implementation consumer |
| `crates/omo/components/maho-omo-lsp` | 1 | Native Rust implementation consumer with lsp-core and lsp-daemon |
| `crates/omo/tmux-core` | 1 | Native Rust implementation consumer |

### Non-Crate / Target Consumers & Fixtures (227 rows across 104 targets)

| Target / Consumer Evidence | Verification Required Rows | Scope / Description |
| --- | ---: | --- |
| `inv:layer:0` | 92 | Target contract / inventory / fixture consumer |
| `.gitmodules` | 6 | Target contract / inventory / fixture consumer |
| `.github/FUNDING.yml` | 4 | Target contract / inventory / fixture consumer |
| `assets/omo.schema.json` | 4 | Target contract / inventory / fixture consumer |
| `.agents/AGENTS.md` | 4 | Target contract / inventory / fixture consumer |
| `scripts/AGENTS.md` | 3 | Target contract / inventory / fixture consumer |
| `Cargo.toml` | 3 | Target contract / inventory / fixture consumer |
| `.claude/settings.json` | 3 | Target contract / inventory / fixture consumer |
| `.omo/init-deep.json` | 3 | Target contract / inventory / fixture consumer |
| `.github/workflows/ci.yml` | 2 | Target contract / inventory / fixture consumer |
| `test-env.ts` | 2 | Target contract / inventory / fixture consumer |
| `LICENSES/MIT.txt` | 2 | Target contract / inventory / fixture consumer |
| `README.md` | 2 | Target contract / inventory / fixture consumer |
| `packages/omo-senpi/src/components/formatter/formatter.test.ts` | 2 | Target contract / inventory / fixture consumer |
| `packages/omo-senpi/src/components/gateway/AGENTS.md` | 2 | Target contract / inventory / fixture consumer |
| `packages/omo-senpi/src/components/post-mutation/post-mutation.test.ts` | 2 | Target contract / inventory / fixture consumer |
| `packages/omo-senpi/src/components/x-search/AGENTS.md` | 2 | Target contract / inventory / fixture consumer |
| `bin/AGENTS.md` | 2 | Target contract / inventory / fixture consumer |
| `script/AGENTS.md` | 2 | Target contract / inventory / fixture consumer |
| `tools/check-omo-sites.mjs` | 1 | Target contract / inventory / fixture consumer |
| `tools/parity-audit.mjs` | 1 | Target contract / inventory / fixture consumer |
| `AGENTS.md` | 1 | Target contract / inventory / fixture consumer |
| `assets/AGENTS.md` | 1 | Target contract / inventory / fixture consumer |
| `bun.lock` | 1 | Target contract / inventory / fixture consumer |
| `bun-test.d.ts` | 1 | Target contract / inventory / fixture consumer |
| `bunfig.root.toml` | 1 | Target contract / inventory / fixture consumer |
| `bunfig.toml` | 1 | Target contract / inventory / fixture consumer |
| `bunfig.win2.parallel.toml` | 1 | Target contract / inventory / fixture consumer |
| `bunfig.win2.parallel.windows.toml` | 1 | Target contract / inventory / fixture consumer |
| `bunfig.win2.toml` | 1 | Target contract / inventory / fixture consumer |
| `Cargo.lock` | 1 | Target contract / inventory / fixture consumer |
| `CHANGELOG.md` | 1 | Target contract / inventory / fixture consumer |
| `changes.md` | 1 | Target contract / inventory / fixture consumer |
| `CLA.md` | 1 | Target contract / inventory / fixture consumer |
| `.codex/setup.sh` | 1 | Target contract / inventory / fixture consumer |
| `CONTRIBUTING.md` | 1 | Target contract / inventory / fixture consumer |
| `.cursor/environment.json` | 1 | Target contract / inventory / fixture consumer |
| `.devcontainer/README.md` | 1 | Target contract / inventory / fixture consumer |
| `docs/AGENTS.md` | 1 | Target contract / inventory / fixture consumer |
| `.env.example` | 1 | Target contract / inventory / fixture consumer |
| `.gitattributes` | 1 | Target contract / inventory / fixture consumer |
| `.gitignore` | 1 | Target contract / inventory / fixture consumer |
| `LICENSE.md` | 1 | Target contract / inventory / fixture consumer |
| `.omo/fixtures/releases.json` | 1 | Target contract / inventory / fixture consumer |
| `packages/AGENTS.md` | 1 | Target contract / inventory / fixture consumer |
| `postinstall.test.ts` | 1 | Target contract / inventory / fixture consumer |
| `README.ja.md` | 1 | Target contract / inventory / fixture consumer |
| `README.ko.md` | 1 | Target contract / inventory / fixture consumer |
| `README.ru.md` | 1 | Target contract / inventory / fixture consumer |
| `README.zh-cn.md` | 1 | Target contract / inventory / fixture consumer |
| `ROADMAP.md` | 1 | Target contract / inventory / fixture consumer |
| `rust-toolchain.toml` | 1 | Target contract / inventory / fixture consumer |
| `test-hermetic-home.ts` | 1 | Target contract / inventory / fixture consumer |
| `test-setup.ts` | 1 | Target contract / inventory / fixture consumer |
| `test-support/remove-tree.test.ts` | 1 | Target contract / inventory / fixture consumer |
| `tests/AGENTS.md` | 1 | Target contract / inventory / fixture consumer |
| `THIRD-PARTY-NOTICES.md` | 1 | Target contract / inventory / fixture consumer |
| `tsconfig.json` | 1 | Target contract / inventory / fixture consumer |
| `.devcontainer/devcontainer.json` | 1 | Target contract / inventory / fixture consumer |
| `md:baseline/UPSTREAM-BASELINE.md:B65` | 1 | Target contract / inventory / fixture consumer |
| `bin/version-mismatch.test.ts` | 1 | Target contract / inventory / fixture consumer |
| `scripts/check-third-party-notices.test.mjs` | 1 | Target contract / inventory / fixture consumer |
| `scripts/third-party-notice-requirements.mjs` | 1 | Target contract / inventory / fixture consumer |
| `.github/ISSUE_TEMPLATE/bug_report.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/ISSUE_TEMPLATE/config.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/ISSUE_TEMPLATE/feature_request.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/ISSUE_TEMPLATE/general.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/ISSUE_TEMPLATE/lazycodex_bug_report.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/building-in-public.png` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/elestyle.jpg` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/google.jpg` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/hephaestus.png` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/hero.jpg` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/indent.jpg` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/microsoft.jpg` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/omo-herdr-dag.png` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/omo-icon-light.svg` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/omo-logo.png` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/omo.png` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/opengateway-logo.svg` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/orchestrator-atlas.png` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/sisyphus.png` | 1 | Target contract / inventory / fixture consumer |
| `.github/assets/sisyphuslabs.png` | 1 | Target contract / inventory / fixture consumer |
| `.github/pull_request_template.md` | 1 | Target contract / inventory / fixture consumer |
| `.github/scripts/omo-bun-executable.entitlements` | 1 | Target contract / inventory / fixture consumer |
| `.github/scripts/windows-ci-telemetry.ps1` | 1 | Target contract / inventory / fixture consumer |
| `.github/scripts/write-job-summary.sh` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/bot-merge.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/cla.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/compiled-worker.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/get-worker-ci.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/get-worker-deploy.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/isolation-linux-fs.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/lint-workflows.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/npm-dist-tag-rollback.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/package-labels.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/refresh-model-capabilities.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/review-claims.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/sisyphus-agent.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/stats.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/web-ci.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/web-deploy.yml` | 1 | Target contract / inventory / fixture consumer |
| `.github/workflows/windows-flake-soak.yml` | 1 | Target contract / inventory / fixture consumer |

---

## Exclusions & Deferred Controlling Decisions

Every row classified as `closed_scope` (1,291 rows total) cites an authoritative controlling decision:
* **`scope-opencode-excluded`**: 632 rows total (4 in Shared-Core, 353 in Memory-Config, 275 in Interfaces).
* **`dec-a-02`**: 600 rows in Interfaces (browser/desktop platforms, engines and permission protocol deferred by owner).
* **`dec-a-05`**: 50 rows in Interfaces (updater, signing, download trust, service and binary distribution matrix deferred by owner).
* **`dec-sc-claude-code-deferred`**: 5 rows in Shared-Core (Claude Code agent/command/plugin/MCP loaders deferred by owner).
* **`dec-a-04-thread-sdk`**: 2 rows in Interfaces (thread/SDK compatibility surface deferred by owner).
* **`DEC-MC-B70`**: 1 row in Memory-Config (Published schema generation asset policy).
* **`DEC-C-A3`**: 1 row in Memory-Config (Agent-dir setup/import five-entry copy policy).
* **`exclusions_without_decision`**: **0**.
