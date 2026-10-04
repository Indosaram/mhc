# Negative fixture: parity fragment with an intra-table blank line

This reproduces the Task 8 defect: `crates/maho-cli/parity.d/38.md` had a blank line at line 88
inside a `TS path` table, so `parseFragment` stopped at the blank and silently dropped every row
after it (85 parsed of 155 declared = 70 hidden). The verifier's declared-vs-parsed guard MUST
detect this. A parser-green audit is NOT proof that the ledger is complete.

| TS path | Rust path | TS tests | Rust tests | status |
|---|---|---:|---:|---|
| before.ts | src/before.rs | 1 | 1 | done |

| hidden-1.ts | src/hidden_1.rs | 1 | 1 | done |
| hidden-2.ts | src/hidden_2.rs | 1 | 1 | done |
