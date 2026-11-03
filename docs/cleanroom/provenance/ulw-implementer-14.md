# Provenance: ulw-implementer-14 (crates/maho-ultrawork engine)

- Fresh clean-room session, MIT. Source of behavior: `docs/cleanroom/specs/ultrawork.md` (sections 1-8, test vectors) and `docs/cleanroom/specs/README-ulw.md`.
- `build.rs` size caps derive from `docs/cleanroom/specs/ultrawork-contract.md` section 4 (size budget only). The three denylist tokens came from the plan/task text, not from any spec file.
- Mass block is about 1.4 KB (1,411 bytes) against the spec's about 1.5 KB; arming state is process-memory only (resumed sessions in a new process get the full contract again). Both are noted in `crates/maho-ultrawork/PROVENANCE.md`.
- MIT API shape read from `crates/maho-ext-api/src/types.rs`, `crates/maho-skills/src/extension.rs`, `crates/maho-task/Cargo.toml`, `crates/builtins/maho-ext-todotools`, `crates/builtins/maho-ext-goal`.
- Never opened: `crates/omo/**`, `/home/indo/code/oh-my-openagent`, `~/.bun/install/cache/omo-ai*`, `.omo/**`, other worktrees, git history outside this repo. No SUL access.
- No cargo run in this session; the merged gate (todo 18) compiles and tests.
