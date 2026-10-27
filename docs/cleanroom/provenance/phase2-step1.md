# Phase 2 step 1 — comment checker and LSP (implementer notes)

- Worktree: `/workspace/work/mhc-clean` (branch `cleanroom/phase1`, fast-forwarded to
  `mit/skills-relicense` @ ac9727cc before starting). The SUL paths listed in `phase1-env.txt`
  were absent from the working tree (skip-worktree; only `crates/omo/**` Cargo manifests with stub
  targets present so Cargo can load the workspace) and were not opened.
- Sources read (all MIT, LICENSE checked first; cloned to `/workspace/work/upstream`):
  - `code-yeongyu/pi-comment-checker` @ 0a38dd8ff362be1b6020f2baba7b5723cbc5ea76
  - `code-yeongyu/go-claude-code-comment-checker` @ 311565fc9380579449ae34a8ab4c2cd386da840c
  - `code-yeongyu/pi-lsp-client` @ fc8f6124d0022ba8e6ed4e0678007c42d0f5e74e
  - `can1357/oh-my-pi` @ f59bbac3d56e8b485fef466388b1cb5be4b93123 (`packages/coding-agent/src/lsp/mux/*` only)
- MIT call sites read in the tree: `crates/extensions/maho-ext-pi-comment-checker/**`,
  `crates/maho-cli/src/cli/{extension_registry,default_extensions}.rs`, `crates/maho-ext-api`,
  `crates/maho-tools/src/definition.rs`, `crates/extensions/maho-ext-pi-ast-grep/src/{index,tools}.rs`,
  `tools/package-native.mjs`, `scripts/mit-profile-smoke.py`.
- No behavior spec step was needed: every ported piece has an MIT original. The formatter /
  post-mutation hook traits (`maho_lsp::hooks`) are a new neutral interface, not a port.
- Results: `crates/maho-comment-checker` (P), `crates/maho-lsp` (P; daemon P-design; hooks C);
  see each crate's PROVENANCE.md and `parity.d/maho-lsp.md`.
