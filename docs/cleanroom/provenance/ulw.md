# ulw workflow overhaul - clean-room implementer environment

- Spec writers and reviewers (SUL readers) worked in `/home/projects/mhc-wt/ulw-overhaul`
  (branch `ulw/overhaul`). The ultrawork specs and their leak review are rules in
  `docs/cleanroom/specs/README-ulw.md`.
- Implementers get a history-free export, not a worktree: `/home/projects/mhc-wt/ulw-cleanroom`,
  branch `cleanroom/ulw`, built from `ulw/overhaul` @ `1f8a4df7b6961cb92c8df58a664f3e6dfc29a2cf`
  with `git archive <rev> -- <allowlist>` and `git init -b cleanroom/ulw` + one root commit.
  It has no remote, no alternates and no access to the main object store.
- Export root commit: `96d132627c49eb730a05294200be9b9d48f22ce1`. Todo 17 transfers
  `96d132627c49eb730a05294200be9b9d48f22ce1..HEAD` as patches (no merge, no fetch).
- 11016 tracked paths, 6463 exported, 4553 excluded. Each excluded path and its reason is listed
  in `ulw-env.forbidden`:
  - `crates-omo` (3487): `crates/omo/**`
  - `dot-omo` (651): `.omo/**`
  - `cleanroom-reviews` (30): `docs/cleanroom/reviews/**`
  - `spec-integration-trace` (2): `docs/cleanroom/specs/*-integration.md`, `*-trace.md`
  - `docs-audit` (7): `docs/audit/**`
  - `named-path` (4): `assets/omo-latest/manifest.json`, `assets/omo-latest/PROVENANCE.md`,
    `assets/omo.schema.json`, `docs/cleanroom/phase4-5-inventory.md`
  - `sul-pool-doc` (1): `assets/omo-latest/skills/AGENTS.md` (SUL per `MIT-SKILLS.md`)
  - `omo-only-maho-cli` (17): the `#[cfg(feature = "omo")]` modules of
    `crates/maho-cli/src/cli/mod.rs` and `crates/maho-cli/src/lib.rs` named in `LICENSE`, plus
    their `#[path]` children (`task_host_connection.rs`)
  - `omo-parity-record` (9): `parity.d` records that cite or quote pinned oh-my-openagent
    sources for the omo-only modules
  - `phase4-forbidden` (23): `phase4-env.forbidden` entries outside `assets/omo-latest/skills/`
    still tracked (OmO-only `tools/*` scripts, `crates/maho-cli/src/import_omo.rs`). That
    list's skill-pool entries are superseded by the skill rules here.
  - `omoa-attributed-skill` (158): skill dirs whose `ATTRIBUTION.md` names oh-my-openagent
    (`ulw-research`, `ultimate-browsing`)
  - `sul-subtree` (163): `frontend/scripts/detect/**` and
    `frontend/references/perfection/detect.md` (SUL-1.0 per `MIT-SKILLS.md`)
  - `ulw-bench-nonsynthetic-fixture` (1): `tools/ulw-bench/fixtures/session-*.jsonl` (a
    real-session snapshot). No other file is under `tools/ulw-bench/fixtures/`. The MIT
    `tools/ulw-bench/*.mjs` scripts and `scenarios/` generators are included.
  - Any file that still had a SUL SPDX/`license =` marker was also excluded (none remained
    after the rules above).
- Export-only edit: the root `Cargo.toml` loses its 84 workspace `members` and
  `[workspace.dependencies]` lines pointing into `crates/omo/`. The export is NOT
  cargo-buildable as-is: 14 optional path deps point into the absent `crates/omo` (13 in
  `crates/maho-cli/Cargo.toml:118-155`, 1 in
  `crates/extensions/maho-ext-pi-comment-checker/Cargo.toml:18`), and cargo resolves optional
  path deps too. This is intended: todos 14-16 do not run cargo. Their tests are written in the
  export and run once in the parent's merged gate (todo 18).
- The plan-mandated MIT input `assets/omo-latest/skills/ultrawork/SKILL.md` shares much wording
  with the SUL directive, so todo 15's overlap gate against `directive.txt` stays strict (0
  8-word runs). The todo 15 writer uses the existing `SKILL.md` only for facts (frontmatter,
  names, paths, keywords) and writes all prose fresh.
- `ulw-env.allowlist`: every exported path with a reason. `mentions-only` (52) marks files that
  only name oh-my-openagent (disclaimers, provenance, license tooling, pins, docs, the
  `give-me-tips`/`onboarding` banned-word checks). None carries SUL code.
- `ulw-env.sha256`: sha256 of every file in the export as committed (trimmed `Cargo.toml`).
- Implementers (todos 14-16) are launched as new `task` children with cwd = the export, a prompt
  holding only the spec path and the rules, and no `load_skills` except `programming`. They
  must not open the excluded paths, any other mhc checkout or its git history, or any
  OmO/oh-my-openagent/lazycodex copy.

## Launch prompts

Recorded verbatim by the orchestrator when it launches todos 14-16.

### Todo 14

_(to be filled in by the orchestrator)_

### Todo 15

_(to be filled in by the orchestrator)_

### Todo 16

_(to be filled in by the orchestrator)_
