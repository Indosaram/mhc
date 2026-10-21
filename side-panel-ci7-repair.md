# CI7 side-panel repair

Worktree: `/home/projects/mhc-wt/nonwindows-20261020`

Source scope: `crates/omo/components/maho-omo-side-panel` only.

Input: `/home/projects/mhc/.omo/nonwindows-20261020/ci-39428472935/clippy.log`.

## Changes

Addressed the 15 reported crate diagnostics with source-only edits:

| File under crate `src/` | Original CI lines | Lint count | Repair |
| --- | --- | --- | --- |
| `format.rs` | 26, 33, 72 | 3 | Separate adjacent independent `if` statements onto new lines. |
| `rows.rs` | 12 | 1 | Separate block-skipping and separator insertion guards. |
| `runtime.rs` | 40, 45, 75 (twice) | 4 | Separate epoch guards from update guards; put each independent teardown action on its own line. |
| `host_surface.rs` | 129, 250 | 2 | Collapse URL-hook installation and layout restoration guards into short-circuit let chains. |
| `native_readers.rs` | 274 | 1 | Combine the park-file binding and reflection status predicate; retain parentheses around the disjunction. |
| `sections.rs` | 117, 145 | 2 | Collapse running-child activity and home-directory abbreviation guards. |
| `usage.rs` | 464, 465 | 2 | Combine the Codex-only condition, decoded claims binding, and account ID binding into one let chain. |

The eight `possible_missing_else` repairs use newlines, not `else`, so both independent branches still execute when applicable. The seven `collapsible_if` repairs retain left-to-right short-circuit evaluation, tuple evaluation for the URL hook, existing branch bodies, output strings, and side-effect order. The usage header remains conditional on Codex plus successful claims/account ID extraction. No lint suppressions, dependencies, public API changes, or test changes were introduced.

## Evidence and limits

- Read the supplied log, including all side-panel diagnostics and the other crate errors; edits are restricted to the side-panel crate plus this requested report.
- All seven source updates and this report were saved through `apply_patch`.
- No cargo, tests, CI, commits, formatter, or diagnostics were run, as requested. Compilation, lint cleanliness, and live display/runtime behavior are unverified; this is not a passing CI claim.
- The log also reports eight errors in `maho-core`; those are outside this repair scope and were not changed.
