# Phase 4 - general features rebuilt clean-room (environment and roles)

- Spec writers and reviewers (SUL readers) worked in `/home/projects/mhc-worktrees/mit-phase5`
  (branch `mit/phase5`). One writer session per unit wrote `docs/cleanroom/specs/<unit>.md`; a
  different reviewer session ran the README leak checklist on each spec against the SUL source and
  fixed what it found. Review records are kept in `docs/cleanroom/reviews/` and are not implementer
  inputs. Specs committed at `290858e4` and `8f3d2e29`.
- Units: workflow-dag, workpool, task-control, ast-grep, claude-plugin-hooks, post-edit-format,
  question-gates, feedback, side-panel, x-search, git-bash-mcp (see
  `docs/cleanroom/phase4-5-inventory.md` for why these and only these).
- Implementers worked in `/home/projects/mhc-worktrees/mit-impl` (branch `cleanroom/impl`), built
  from `mit/phase5` @ `8f3d2e2926cdd50694b8962831679c23d36db258`. 4575 forbidden files were removed
  from the working tree and marked skip-worktree before any implementer started: `crates/omo/**`,
  `assets/omo-latest/**`, `assets/omo.schema.json`, `.omo/**`, `docs/cleanroom/reviews/**`,
  `docs/cleanroom/phase4-5-inventory.md`, the OmO-only `tools/*` scripts and the omo-only
  `maho-cli` modules. Environment manifest: `phase4-env.sha256` (sha256 of every remaining tracked
  file) and `phase4-env.forbidden` (the removed list).
- Each implementer session was a fresh session that had not read SUL, was given one spec plus the
  MIT tree, was forbidden from opening the paths above, git history of removed paths, or any
  OmO/oh-my-openagent/lazycodex copy, and wrote a `PROVENANCE.md` in its crate (method C).
- Integration (workspace members, CLI registration) was applied by the parent from each
  implementer's reported integration list; the parent only wires crates and does not edit their
  behaviour.
