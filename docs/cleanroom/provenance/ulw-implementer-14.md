# Provenance: ulw-implementer-14 (crates/maho-ultrawork engine)

- Fresh clean-room session, MIT. Source of behavior: `docs/cleanroom/specs/ultrawork.md` (sections 1-8, test vectors) and `docs/cleanroom/specs/README-ulw.md`.
- Size/denylist gate in `build.rs` derives from `docs/cleanroom/specs/ultrawork-contract.md` section 4.
- MIT API shape read from `crates/maho-ext-api/src/types.rs`, `crates/maho-skills/src/extension.rs`, `crates/maho-task/Cargo.toml`, `crates/builtins/maho-ext-todotools`, `crates/builtins/maho-ext-goal`.
- Never opened: `crates/omo/**`, `/home/indo/code/oh-my-openagent`, `~/.bun/install/cache/omo-ai*`, `.omo/**`, other worktrees, git history outside this repo. No SUL access.
- No cargo run in this session; the merged gate (todo 18) compiles and tests.
