# Task 34 evidence - maho-ai cancel_deferred fixtures
Worker st_01a1207f changed fixture model ids only; orchestrator verified and committed.
Root cause: three tests called get_builtin_model(provider, "") which no longer resolves in the generated catalog.
Pinned TS (senpi fe8c564b packages/ai/test/providers.test.ts) builds a synthetic testModel; the Rust fixtures only need any real base model of that provider, so claude-opus-4-8 / gpt-4o keep the asserted behavior (provider resolution, capability rejection, option forwarding).
Verification (orchestrator): cargo nextest run -p maho-ai --no-fail-fast: 2028 passed, 0 skipped, rc 0; cargo clippy -p maho-ai --all-targets -- -D warnings rc 0.
Cleanup: none needed.