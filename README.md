# mhc

mhc is a native Rust port of the senpi coding agent and its terminal interface, with the oh-my-openagent (omo) features ported on top. It aims to look, type and behave exactly like omo on senpi today: senpi is the only upstream, ported file-for-file so upstream changes stay a mechanical translation, and golden fixtures generated from senpi itself prove the output is identical. The engine contains no codex-rs code, extensions are native Rust crates registered statically, and configuration lives in `~/.maho/agent` (`mhc import-omo` copies an existing `~/.omo` setup). Source pins are in [PINS.md](PINS.md), contributor and agent conventions in [AGENTS.md](AGENTS.md), and licenses in [LICENSES/](LICENSES/).

Existing crate names and configuration paths are retained.

## Install

`mhc` runs from a staged directory that holds the binary plus its binary-sibling runtime resources:
the pinned builtin skills and the native `ast-grep-mcp` MCP server. Both are resolved relative to
the executable, so a distribution is a single self-contained directory.

1. Build every workspace binary (this produces `mhc` and `ast-grep-mcp`):

   ```bash
   cargo build --workspace --bins --release
   ```

2. Stage a distribution directory from the built binaries:

   ```bash
   bun tools/package-native.mjs --binary target/release/mhc --output dist/mhc
   ```

   The script copies `mhc` and its sibling `ast-grep-mcp`, stages the pinned builtin skills under
   `skills/` and the license texts under `licenses/`, and writes `manifest.json` recording the
   sha256, size and mode of every staged file plus the versions and source pins it was built from.
   `--omo-root` selects the pinned oh-my-openagent checkout (default `$OMO_SRC`); `--ast-grep-mcp`
   overrides the helper binary path; `--force` overwrites an existing non-empty staging directory.

3. Verify a staged directory against its manifest:

   ```bash
   bun tools/package-native.mjs --verify-only dist/mhc
   ```

The runtime resolves resources relative to the executable: skills at `<dir>/skills` (overridable
with `OMO_SENPI_SKILLS_ROOT`), and the ast-grep MCP server at `<dir>/ast-grep-mcp`; the mount warns
when the skills directory is absent and skips the ast-grep server when the helper is missing, so a
staged directory that fails `--verify-only` is not a working install. `bun tools/package-native.mjs
--self-test` exercises the staging and verification logic against a fixture.

## License

This repository is **mixed-licensed**: each part keeps the license of the upstream
project it ports.

- `crates/maho-*`, `crates/builtins`, `crates/extensions` and `crates/vendor` port
  [senpi](https://github.com/code-yeongyu/senpi) and are licensed **MIT** -
  see [LICENSES/MIT.txt](LICENSES/MIT.txt).
- `crates/omo/**` ports [oh-my-openagent](https://github.com/code-yeongyu/oh-my-openagent)
  and is licensed under the **Sustainable Use License 1.0** -
  see [LICENSES/SUL-1.0.md](LICENSES/SUL-1.0.md). It permits free, non-commercial use,
  modification and redistribution; commercial use and paid distribution are not permitted.

This software is a modified derivative of both upstream projects (a Rust reimplementation
of their TypeScript sources), which the Sustainable Use License requires to be stated.
