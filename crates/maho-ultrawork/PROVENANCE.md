# Provenance — maho-ultrawork Contract

- **License**: MIT.
- **Method**: Clean-room (**C**) authoring from specification `docs/cleanroom/specs/ultrawork-contract.md` and structural facts from MIT `assets/omo-latest/skills/ultrawork/SKILL.md`.
- **Inputs Used**:
  - `docs/cleanroom/specs/ultrawork-contract.md` (clean-room requirements, rule inventory, size budgets, scenario specifications).
  - `assets/omo-latest/skills/ultrawork/SKILL.md` (permitted for frontmatter metadata, skill name, and file path facts only).
  - `assets/omo-latest/skills/visual-qa/SKILL.md` (referenced for the bundled terminal capture script `scripts/tui-capture.mjs`).
  - `tools/ulw-bench/check-skill-refs.mjs` (reference validation test harness).
- **Inputs Strictly Excluded (Clean-Room Boundary)**:
  - No `crates/omo/**` sources or history anywhere.
  - No `directive.txt` or SUL references were opened, read, or quoted.
  - No `/home/indo/code/oh-my-openagent` or `~/.bun/install/cache/omo-ai*` paths.
  - No repository git history outside the clean-room export.
- **Deliverables Authored in Fresh Prose**:
  - `crates/maho-ultrawork/contract/core.md`: Complete core execution contract covering all 45 rules with disposition `core` and `condensed-core`, measured at 13,894 bytes, within the spec cap (14,000-16,000 is a target, not a floor).
  - `crates/maho-ultrawork/contract/references/qa-channels.md`: Reference covering manual QA, protocol-level HTTP, browser, desktop GUI, and visual captures.
  - `crates/maho-ultrawork/contract/references/delegation-and-teams.md`: Reference covering subagent dispatch, disjoint write scopes, task boards, and adversarial falsification.
  - `crates/maho-ultrawork/contract/references/commits.md`: Reference covering conventional commit formatting and explicit authorization rules.
  - `crates/maho-ultrawork/contract/references/review-gate.md`: Reference covering dual-trigger reviewer dispatch, delta re-reviews, and human escalation.
  - `crates/maho-ultrawork/contract/references/tui-visual-qa.md`: Reference covering pseudoterminal capture via `scripts/tui-capture.mjs`, truecolor rendering, and glyph verification.
  - `crates/maho-ultrawork/contract/COVERAGE.md`: Explicit mapping of every core/condensed-core rule (R1-R77) to its section heading in `core.md`.
  - `assets/omo-latest/skills/ultrawork/SKILL.md`: Rewritten with frontmatter, complete core contract, and on-demand reference loading table.
  - `assets/omo-latest/skills/ultrawork/references/*.md`: Shipped references synchronized with contract references.

# Provenance — maho-ultrawork Engine

- **License**: MIT.
- **Method**: Clean-room (**C**) implementation from `docs/cleanroom/specs/ultrawork.md` (sections 1-8 and the test vectors) in a fresh session.
- **Inputs read**:
  - `docs/cleanroom/specs/ultrawork.md` and `docs/cleanroom/specs/README-ulw.md` (interface facts: message type, reminder tags, `ultrawork-disabled`).
  - `docs/cleanroom/specs/ultrawork-contract.md` (size budget for `build.rs`) and the contract files under `crates/maho-ultrawork/contract/` (consumed via `include_str!`, never edited).
  - MIT tree for API shape only: `crates/maho-ext-api/src/types.rs` (events, results, flags, `CustomMessage`), `crates/maho-skills/src/extension.rs` (handler registration pattern), `crates/maho-task/Cargo.toml` (manifest pattern), `crates/builtins/maho-ext-todotools` and `maho-ext-goal` (shape of the `todo` tool result details), `tools/ulw-bench/` (file names only).
  - `assets/omo-latest/skills/ultrawork/` (shipped copy checked against `contract/`).
- **Inputs strictly excluded**: no `crates/omo/**`, no `/home/indo/code/oh-my-openagent`, no `~/.bun/install/cache/omo-ai*`, no `.omo/**`, no git history outside this export. This session never read or had access to any SUL source for the ultrawork engine.
- **Authored in fresh prose/code**: all of `src/**`, `build.rs`, and `tests/**`, including the re-mention notice, the mass block and the todo advisory texts.
- **Not built (by design)**: no context-transform handler, because there is a single contract variant for all models.
