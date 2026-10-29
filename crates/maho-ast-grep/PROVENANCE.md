# Provenance — maho-ast-grep

- License: MIT.
- Method: **C** (clean room) from `docs/cleanroom/specs/ast-grep.md` at spec commit `8f3d2e29`. It
  replaces the SUL `ast-grep-mcp` crate and `maho-omo-ast-grep` component with the `mhc-ast-grep`
  binary and the `maho-ast-grep` library.
- Allowed inputs this session used:
  - the spec `docs/cleanroom/specs/ast-grep.md` and `docs/cleanroom/specs/README.md`;
  - MIT code already in this checkout: `AGENTS.md`, the root `Cargo.toml` (dependency pins and
    lints), `crates/maho-ext-api` (`McpServerDeclaration`, `ExtensionApi`, `diagnostic`),
    `crates/maho-git-bash` (sibling-binary crate layout, extension shape, PROVENANCE layout, test
    layout), `crates/maho-mcp-builtins` (extension shape), `crates/maho-cli/src/cli/default_extensions.rs`
    and `cli/git_tools.rs` (registration pattern);
  - the public behaviour of the installed `ast-grep` command line (`ast-grep run`, `scan`,
    `--json=stream`, `--update-all`, `--version`), observed by running it on scratch files.
- No forbidden input was read: nothing under `crates/omo/**` (absent from this checkout),
  `assets/omo-latest/**`, `assets/omo.schema.json`, `.omo/**`, no oh-my-openagent / OmO /
  lazycodex source or URL, no git history of removed paths, no other copy of this repository, and
  none of the omo-gated `maho-cli` modules listed in the specs README.
- Tests are new, written from the spec's numbered acceptance tests (§8); no OmO fixtures or parity
  vectors were reused. Tests that need the real `ast-grep` fail when it is missing; they do not
  skip. Fake-program tests (16, 17, 18, 19, 24, 11) are `cfg(unix)` shell scripts.
