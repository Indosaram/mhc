# PROVENANCE.md

This unit was implemented clean-room from `docs/cleanroom/specs/mcp-auth-hardening.md` plus the listed public/MIT sources:
- RFC 6750 §3 (Bearer `WWW-Authenticate`, `insufficient_scope`)
- RFC 6749 §3.3 (scope syntax)
- RFC 8707 (resource indicators)
- RFC 9728 (protected resource metadata)
- RFC 8414 (authorization server metadata)
- MCP specification "Authorization" section
- MIT `@modelcontextprotocol/sdk` TypeScript client (`client/auth.ts`, `client/streamableHttp.ts`, `client/stdio.ts`)

It was implemented in the clean-room checkout `/home/projects/mhc-worktrees/mcp-auth-impl` without any other SUL paths.

Created/Changed:
- `crates/builtins/maho-ext-mcp/src/auth/challenge.rs` (created)
- `crates/builtins/maho-ext-mcp/src/connection.rs` (changed)
- `crates/builtins/maho-ext-mcp/src/transport.rs` (changed)
- `crates/builtins/maho-ext-mcp/src/transport_sdk.rs` (changed)
- `crates/builtins/maho-ext-mcp/src/auth/oauth.rs` (changed)
- ... (other files to be updated)

## Lane A
- `crates/builtins/maho-ext-mcp/src/auth/oauth.rs` (resource indicator, discovery, process cache, HTTPS rule)
- `crates/builtins/maho-ext-mcp/src/auth/oauth_provider.rs` (require_https, escalated/effective scopes, discovery reset)
- `crates/builtins/maho-ext-mcp/src/auth/token_store.rs` (discoveryState removed, legacy key dropped on read)
- `crates/builtins/maho-ext-mcp/src/auth/oauth_refresh.rs` (process-wide coalescing, force_refresh, observers)
- `crates/builtins/maho-ext-mcp/src/auth/commands_auth.rs` (forced loopback auth)
- `crates/builtins/maho-ext-mcp/src/auth/challenge.rs` (challenge parsing, step-up predicate, scope merge)
- `crates/builtins/maho-ext-mcp/tests/{auth_resource,auth_discovery,auth_https,auth_challenge_parse,auth_refresh_coalescing}.rs`, plus adapted `oauth.rs`, `oauth_flow.rs`, `oauth_client_auth.rs`, `auth_announcement.rs`

## Lane B

Implemented from spec sections 4.5-4.7, 5, 7, 8, 9 and test cases 6, 7, 8, 10, 11, 12.

Source files:
- `src/transport_sdk.rs` (step-up loop, forced refresh after rejection, error cause fields, URL redaction in send errors)
- `src/transport.rs` (handler carried through `McpTransportConnection`, stdio host env filter)
- `src/connection.rs` (`ServerConnection::set_auth_challenge_handler`)
- `src/service_connection.rs` (production handler wiring)
- `src/diagnose.rs` (stdio host env filter in the diagnosis re-run)
- `src/log.rs` (`redact_url`, `redact_urls_in_text`, file sink retry with injectable clock)
- `src/lib.rs`, `src/auth/mod.rs` (module registration)
- `src/stdio_env.rs` (new)
- `src/auth/challenge_login.rs` (new)

Tests (new): `tests/transport_step_up.rs`, `tests/transport_refresh_rejection.rs`, `tests/transport_redaction.rs`, `tests/stdio_env_filter.rs`, `tests/log_sink_retry.rs`, `tests/challenge_login.rs`, fixture `tests/fixtures/env-report.mjs`; registered in `tests/main.rs`.
