# MIT-only licensing

mhc is MIT-licensed. There is one build: the `mhc` binary from `maho-cli` (plus `mhc-launcher`,
`mhc-update-probe`, the `mhc-lsp` daemon and the `senpi-desktop-engine` sibling binary) links only
MIT code. Ports follow MIT upstreams (senpi, oh-my-pi, pi-* packages, omowright); the remaining
parts were written clean-room from the specs in `docs/cleanroom/specs`. What the binary ships is
listed in [features.md](features.md).

The earlier oh-my-openagent port and the features that existed only there are not part of mhc.
mhc still reads the shared on-disk configuration (`~/.omo/agent`, `~/.omo/omo.jsonc`); see
[configuration.md](configuration.md).

## How it is enforced (CI job `mit-profile`)

1. `cargo deny ... --config deny-mit.toml check licenses` checks the dependency graph against the
   MIT-compatible license allowlist, with workspace crates included
   (`[licenses.private] ignore = false`).
2. `cargo clippy -p maho-cli --lib --bins -- -D warnings` and a build.
3. `python3 scripts/mit-profile-smoke.py target/debug/mhc` drives the built binary end to end
   against a loopback model and checks that the shipped tools, commands and subsystems listed in
   [features.md](features.md) work.
4. `cargo nextest run -p maho-cli`.

`bun tools/package-native.mjs --mit --output <dir> [--binary target/release/mhc]` stages the
release payload: the MIT executables, the MIT license text and third-party notices, and a manifest
with `profile: "mit"`. `--verify-only` re-checks a stage. `tools/license-audit.mjs` requires every
workspace crate to be MIT-licensed.
