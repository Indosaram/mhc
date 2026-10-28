# Provenance - maho-format

- License: MIT.
- Method: **C** (clean room). Built from `docs/cleanroom/specs/post-edit-format.md`
  (spec commit 8f3d2e29), with `docs/cleanroom/specs/config-keys.md` (`formatOnMutation` keys)
  and `docs/cleanroom/specs/README.md` (roles and rules).
- Allowed inputs read in the implementing session:
  - those spec files;
  - MIT code in this checkout: `crates/maho-lsp` (`hooks.rs`, `post_edit.rs`, `extension.rs`,
    `config.rs` binary resolver and server lookup, `client.rs`, `manager.rs`, `tools.rs`,
    `edit.rs`, `types.rs`), `crates/maho-config` (`load_native`, schema of
    `formatOnMutation`), `crates/maho-ext-api` (tool-result event and context types),
    `crates/builtins/maho-ext-gpt-apply-patch` (shape of the `apply_patch` result details),
    `crates/maho-gateway` (settings loading and PROVENANCE layout), `crates/maho-cli/src/cli/default_extensions.rs`
    (registration pattern), the root `Cargo.toml` and `AGENTS.md`.
- No forbidden input was read: nothing under `crates/omo/**` (absent from this checkout),
  `assets/omo-latest/**`, `assets/omo.schema.json`, `.omo/**`, any oh-my-openagent / OmO /
  lazycodex source or URL, git history of removed paths, or another machine's copy of the repo.
- Message wording, identifiers and module layout are new. Tests in `tests/acceptance.rs` were
  written from the spec's numbered acceptance tests; no upstream or OmO fixtures were reused.
