# Provenance - maho-feedback

- License: MIT.
- Method: **C** (clean room). Built from `docs/cleanroom/specs/feedback.md` at spec commit
  `8f3d2e29` (the file's last change, `290858e4`, is contained in it), plus the role and naming
  rules in `docs/cleanroom/specs/README.md` that it is governed by.
- Allowed inputs of the implementing session (and the only ones used):
  - the spec files above;
  - MIT code already in this checkout, read for the APIs this crate calls or follows as a
    pattern: `crates/maho-ext-api` (extension, command, RPC handler, UI dialog and
    upload-redactor/host-version APIs), `crates/maho-core` (`share::upload_redaction`, the
    `ExtensionActions` implementation and session-entry shape), `crates/maho-ai` (message
    content block wire names), `crates/maho-config` (`load_native`, `process_env`),
    `crates/maho-gateway`, `crates/maho-context-files` and `crates/maho-telemetry` (crate layout,
    extension wiring, `PROVENANCE.md` form), `crates/maho-cli/src/cli/default_extensions.rs` and
    sibling MIT mount modules (how MIT extensions are registered), the root `Cargo.toml`
    (workspace dependencies and lints);
  - public documentation of the `jiff`, `regex`, `reqwest`, `gethostname` and `uuid` crates
    (their source in the local cargo registry, for signatures only).
- No forbidden input was read: nothing under `crates/omo/**`, `assets/omo-latest/**`,
  `assets/omo.schema.json` or `.omo/**`; no omo-gated `maho-cli` module; no OmO /
  oh-my-openagent / lazycodex / COMP checkout or URL; no git history of removed paths; no copy of
  the repository on another machine; no output of running SUL code.
- All identifiers, messages, section titles, the config word lists and the log-field allowlist
  are new. Interface facts (RPC names, JSON field names, section ids, result and refusal values,
  environment variable names, on-disk paths) are reproduced from the spec as it requires.
- Tests (`tests/spec.rs`) were written from the spec's numbered acceptance tests; no upstream or
  OmO fixtures were reused.
