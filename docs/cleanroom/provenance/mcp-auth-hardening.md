# MCP auth hardening - strip and clean-room rebuild

- Strip (parent, SUL reader): the OmO-derived MCP auth code added to the MIT crate
  `crates/builtins/maho-ext-mcp` after `f6a67a82` was removed. `auth/oauth.rs`,
  `auth/commands_auth.rs` and `auth/token_store.rs` are back at `f6a67a82`; `auth/oauth_provider.rs`
  is `f6a67a82` with client_name `mhc`; `auth/oauth_refresh.rs` is `f6a67a82` plus the senpi-era
  `spawn_blocking` token read (ab4abbc7). Deleted: `auth/step_up.rs`, `auth/step_up_login.rs`,
  `auth/resource_indicator.rs`, `env_cleaner.rs`, `parity.d/1003.md` and the tests that covered them
  (oauth_step_up*, oauth_discovery_cache, oauth_transport_hardening, stdio_environment,
  registered_command_auth). `transport_sdk.rs`, `transport.rs`, `connection.rs`,
  `service_connection.rs`, `service.rs`, `diagnose.rs` and `log.rs` lost the step-up, retry, env
  filter, URL redaction and sink-retry code; later senpi/MIT changes to those files are kept.
- The SUL consumer (omo skill MCP manager) keeps its behavior through
  `crates/omo/maho-omo/src/skill_mcp_manager/oauth_compat.rs` (SUL-1.0): scope merge, URL
  redaction, browser launch and forced interactive re-login.
- Spec: `docs/cleanroom/specs/mcp-auth-hardening.md`, written from RFC 6749/6750/8414/8707/9728,
  the MCP authorization specification and the MIT `@modelcontextprotocol/sdk` client.
- Implementer: a fresh session that had not read SUL, working in a standalone checkout built from
  this branch at `d7625a137d16f5a5098771db9eb85d5eb005a993` plus the uncommitted strip, with the paths in
  `mcp-auth-env.excluded` removed (no git history). Environment manifest: `mcp-auth-env.sha256`.
  Its provenance note is `crates/builtins/maho-ext-mcp/PROVENANCE.md`.
- Integration: the parent wires the omo `SkillStepUp` to the implementer's public challenge hook
  and does not edit the implemented behavior.
