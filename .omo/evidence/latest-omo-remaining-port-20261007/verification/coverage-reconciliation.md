# Full-contract coverage reconciliation

Independent read-only worker st_01a11904 compared the original five contract files against all four domain audits and final-ledger.json. It reported 1,844 accounted rows: runtime 7 units, shared-core 111 canonical rows, memory/config 504 records, and interfaces 1,222 unique rows. No unclassified or missing row was reported. This is classification coverage, not behavioral verification.

The worker identified two consumer-mapping defects. The nonexistent crates/omo/lsp-tools-mcp target is corrected to crates/omo/components/maho-omo-lsp, backed by its Cargo dependencies and maho-omo component registration. The aggregate crates/omo/components matrix entry is split into skill-commands (5), bundled-skills (4 additional, 9 total), config-watch (2), config-resolution (1), and config-startup (1). Newly identified packages were added to the running nextest/clippy queue; parity audits are separately recorded. No verification row is closed merely by this correction.

The 227 interfaces rows mapped to 104 non-crate targets require metadata, schema, tooling, or inventory reconciliation evidence, not Cargo test evidence. They remain unresolved until the appropriate checks and source-to-native dispositions are recorded. Worker proposals using --help or file existence alone do not prove behavioral parity.

Scope decisions were reported semantically valid by the worker, including OpenCode exclusion, external compatibility and Thread SDK defer decisions, browser platform backend defer decisions, and updater/signature/download trust defer decisions. The source audit decision links remain the controlling records.
