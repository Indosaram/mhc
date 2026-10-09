# Task 8 evidence - auth storage OAuth and provider api-key auth (T28)
Worker st_01a12028 produced the changes and ended before committing; the orchestrator verified, fixed one clippy lint (unused mut in a test), and committed.
Pinned: /home/indo/code/senpi@fe8c564b packages/coding-agent/src/core/auth-storage.ts (registerOAuthProvider, unregisterOAuthProvider, getOAuthProviders, login, logout, handleLegacyPrompt), packages/ai/src/providers/cloudflare-auth.ts (cloudflareWorkersAIAuth, cloudflareAIGatewayAuth), kimi-coding-auth.ts (kimiCodeApiKeyAuth).
Native: maho-core auth_storage.rs (same method set and order; OAUTH_UNSUPPORTED_ERROR removed), maho-ai providers/{cloudflare_auth,kimi_coding_auth}.rs, auth/oauth/load.rs (BUILTIN_OAUTH_PROVIDERS, load_builtin_oauth).
Verification (orchestrator):
- cargo nextest run -p maho-ai -p maho-core --no-fail-fast: 3134 run, 3131 passed, 0 skipped, 3 failed. The 3 failures (maho-ai *cancel_deferred*) reproduce identically on integration base without this change (fixture model ids missing from generated catalog) -> pre-existing, plan todo 34.
- cargo clippy -p maho-ai -p maho-core --all-targets -- -D warnings: rc 0.
Cleanup: no processes started.