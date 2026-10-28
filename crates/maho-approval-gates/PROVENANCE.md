# Provenance — maho-approval-gates

- License: MIT.
- Method: **C** (clean room). Built from `docs/cleanroom/specs/question-gates.md` at spec commit
  `8f3d2e29`, together with the general rules in `docs/cleanroom/specs/README.md`.
- Allowed inputs read in the implementing session:
  - `docs/cleanroom/specs/question-gates.md` and `docs/cleanroom/specs/README.md`;
  - `AGENTS.md` (workspace conventions);
  - MIT code already in this checkout, for the interfaces this crate plugs into:
    `crates/builtins/maho-ext-ask-user` (tool names, parameter schemas, `required` handling,
    timeout and "do not take the action" result text, its tests as a harness pattern),
    `crates/maho-ext-api` (`Extension`, `ToolCallEvent`, `ExtensionUi`), `crates/maho-ext-host`
    (`emit_tool_call`, loader, runner), `crates/maho-core` (`agent_session` tool-call hook path,
    `session_log`, `brand::env_value`, `output_guard`), `crates/maho-ai` (tool argument
    validation), `crates/builtins/maho-ext-bash-timeout` (a `tool_call` input rewriter),
    `crates/builtins/maho-ext-video-in` tests (session harness), other crates' `PROVENANCE.md`,
    and `crates/maho-cli/src/cli/default_extensions.rs` (registration surface; not edited).
- No forbidden input was read: nothing under `crates/omo/**` (absent from this checkout),
  `assets/omo-latest/**`, `assets/omo.schema.json` or `.omo/**`; no OmO / oh-my-openagent /
  lazycodex source or URL; no git history of removed paths; no other copy of this repository.
- Identifiers, the diagnostic line and the opt-in debug variable (`MAHO_APPROVAL_GATES_DEBUG`) are
  new. The header words, the label words and the two tool names are interface facts reproduced
  from the spec.
- Tests were written from the spec's numbered acceptance tests (§5); no upstream or OmO fixtures
  were reused.
