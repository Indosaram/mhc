# Provenance — maho-git-bash

- License: MIT.
- Method: **C** (clean room) from `docs/cleanroom/specs/git-bash-mcp.md` at spec commit
  `8f3d2e29`. It replaces the SUL `git-bash-mcp` crate and its `omo-git-bash` binary with the
  `mhc-git-bash` binary.
- Allowed inputs this session used:
  - the specs `docs/cleanroom/specs/git-bash-mcp.md`, `docs/cleanroom/specs/README.md` and
    `docs/cleanroom/specs/mcp.md` (§1, the MCP client this server is declared on);
  - MIT code already in this checkout: `AGENTS.md`, the root `Cargo.toml`/`Cargo.lock` (dependency
    pins), `crates/maho-ext-api` (`McpServerDeclaration`, `try_register_mcp_server`, `diagnostic`),
    `crates/maho-mcp-builtins` (extension shape and PROVENANCE layout),
    `crates/maho-git` (crate layout), `crates/maho-lsp` (`mhc-lsp` sibling binary layout and
    sibling lookup beside `current_exe`), `crates/maho-agent/src/harness/pico3/bash.rs`
    (`winsafe` process snapshot calls), `crates/maho-tools/src/bash.rs` (`CREATE_NO_WINDOW` use),
    `crates/maho-cli/src/cli/default_extensions.rs`, `cli/builtin_mcp.rs` and `cli/git_tools.rs`
    (registration pattern), the `tools/package-native.mjs` header comment (sibling naming);
  - public API docs of `tokio`, `tempfile`, `serde_json` and `winsafe` (local cargo registry
    sources) for signatures only.
- No forbidden input was read: nothing under `crates/omo/**` (absent from this checkout),
  `assets/omo-latest/**`, `assets/omo.schema.json`, `.omo/**`, no oh-my-openagent / OmO /
  lazycodex source or URL, no git history of removed paths, no other copy of this repository, and
  none of the omo-gated `maho-cli` modules listed in the specs README.
- Tests are new, written from the spec's numbered acceptance tests (§8) against an injected host;
  no OmO fixtures or parity vectors were reused. Test 10 (W) runs only on Windows with Git Bash;
  `tests/runner_unix.rs` exercises the same runner on unix with `/bin/sh` in place of `bash.exe`.
