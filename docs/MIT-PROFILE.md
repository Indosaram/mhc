# MIT-only licensing

mhc is MIT-licensed. There is one build: the `mhc` binary from `maho-cli` (plus `mhc-launcher`,
`mhc-update-probe`, the `mhc-lsp` daemon and the `senpi-desktop-engine`, `mhc-git-bash` and
`mhc-ast-grep` sibling binaries) links only MIT code. Ports follow MIT upstreams (senpi, oh-my-pi, pi-* packages, omowright); the remaining
parts were written clean-room from the specs in `docs/cleanroom/specs`. What the binary ships is
listed in [features.md](features.md).

The earlier oh-my-openagent port and the features that existed only there are not part of mhc.
mhc still reads the shared on-disk configuration (`~/.omo/agent`, `~/.omo/omo.jsonc`); see
[configuration.md](configuration.md).

## How it is enforced (CI job `mit-profile`)

1. `python3 scripts/check-mit-profile.py` walks the normal dependency graph of every workspace
   member (`cargo tree --workspace`) and fails when a package resolves under `crates/omo/` or names
   a SUL license; `--self-test` checks the gate itself first.
2. `cargo deny --workspace check --config deny-mit.toml licenses` checks the dependency graph of the
   whole workspace against the MIT-compatible license allowlist, with workspace crates included
   (`[licenses.private] ignore = false`).
3. `cargo clippy -p maho-cli --lib --bins -- -D warnings` and a build.
4. `python3 scripts/mit-profile-smoke.py target/debug/mhc` drives the built binary end to end
   against a loopback model and checks that the shipped tools, commands and subsystems listed in
   [features.md](features.md) work.
5. `cargo nextest run -p maho-cli`.

`bun tools/package-native.mjs --binary target/release/mhc --omowright-runtime <dir> --output <dir>`
stages the release payload: `mhc`, `mhc-launcher` and the sibling binaries (`mhc-lsp`,
`senpi-desktop-engine`, `mhc-git-bash`, `mhc-ast-grep`), the skills from `assets/skills/` checked
against `assets/skills/manifest.json`, the browser skill runtime, the MIT license text and
third-party notices, and a hashed manifest. `--verify-only` re-checks a stage.
`tools/license-audit.mjs` requires every workspace crate to be MIT-licensed.
