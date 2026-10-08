# Latest OMO Remaining Native Port

Started: 2026-10-07

## Objective

Audit the full original port contract against current main at 74c79192, classify every unverified row, implement every confirmed in-scope gap through dependency-ordered mass-ulw phase runs, then verify, commit, and push without expanding into explicitly excluded OpenCode/[CC]/OpenClaw or Thread SDK compatibility.

## Tier

HEAVY: this crosses runtime, auth/security, process/session/concurrency, packaging trust, and several crate boundaries.

## Skills

- ultrawork: binding evidence-first execution and stop contract.
- mass-ulw: dependency-ordered phase DAGs and recovery.
- programming (Rust): typed, deterministic Rust implementation discipline.
- git-master: atomic history-aware commits and push delivery.
- visual-qa: load only if the audit confirms a changed TUI/UI surface requiring screenshot review.
- debugging: load only if a behavioral regression appears during the final gate.

## Ideal end state

Every previously unverified/A/U contract row has a current-main consumer and final disposition; all in-scope missing or partial rows are pinned-source-equivalent native Rust with parity evidence; all required gates and real surfaces pass; exclusions cite owner decisions; integration main is clean and equals origin/main.

## Plan and delegation topology

1. Preserve the existing dirty checkout as read-only provenance; use `/home/projects/mhc-wt/main-integrated-latest-omo` as the authoritative integration tree after confirming it is clean and synchronized.
2. Run one audit DAG with bounded read-only lanes for runtime, shared-core, memory/config, and interfaces/packaging. Each lane writes one report under `.omo/evidence/latest-omo-remaining-port-20261007/audit/`; one verifier checks full coverage and zero unclassified rows.
3. Synthesize those reports into one machine-readable ledger, resolving duplicates, exclusions, completed rows, and exact missing/partial units. This judgment stays with the lead.
4. Build one implementation DAG per dependency layer. Each producer owns one disjoint crate/module plus tests and parity fragment; shared Cargo.toml/lib.rs/compose.rs seams serialize through one assembly owner. Every code-changing graph ends in a verification node.
5. Merge accepted atomic lane commits into main in dependency order and fix only integration failures introduced by this work.
6. Run per-crate nextest/clippy/parity with at most four Cargo-building lanes, then one serialized workspace build/package self-test and real-surface QA lane.
7. Self-review the final ledger, diff, and evidence; verify atomic commits with this plan footer; push main and prove local main equals origin/main.

## Success criteria and exact QA scenarios

1. Contract completeness: audit the five contract files and current main. PASS iff the ledger count of unclassified in-scope rows is zero and every exclusion cites a controlling decision. Artifact: `.omo/evidence/latest-omo-remaining-port-20261007/ledger.json` plus audit reports.
2. Implementation/parity: for each changed crate invoke `bun tools/parity-audit.mjs --crate <crate>` and `CARGO_TARGET_DIR=$HOME/.cargo-target/maho-code/<lane> cargo nextest run -p <crate>`. PASS iff exit 0, 0 failed, 0 skipped. Artifacts: per-crate logs.
3. Quality/regression: invoke `CARGO_TARGET_DIR=$HOME/.cargo-target/maho-code/final cargo clippy -p <crate> --all-targets -- -D warnings`, `cargo build --workspace --bins`, and `bun tools/package-native.mjs --self-test`. PASS iff all exit 0 without suppression.
4. Real surface: invoke the package install/verify/residual commands registered in the goal, then `bun script/qa/web-terminal-visual-qa.mjs --title "remaining native port" --command "$E/install/mhc" --input "{Enter}" --evidence-dir "$E/tui"` if available. PASS iff all commands exit 0, screenshot/transcript is nonblank and correctly framed, and cleanup receipts exist.
5. Delivery: `git status --short` is empty and `git rev-parse main` equals `git rev-parse origin/main`. PASS iff the ledger has no open in-scope implementation or verification row.

## Stop

Stop immediately when all five criteria pass, cleanup is complete, every child/run is terminal, and main equals origin/main.

