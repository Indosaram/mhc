# Task 14 evidence - structured facts protocol (T07)
Worker st_01a1205b ported record_fact tool and facts persona assets and stalled before committing; orchestrator verified and committed.
Pinned: oh-my-openagent@adb672532 packages/memory-core/src/facts (assets/facts-persona.md, extraction) and omo-senpi memory record_fact tool.
Verification (orchestrator): cargo nextest run -p maho-memory-core -p maho-omo-memory --no-fail-fast: 1766 passed (1 leaky, pre-existing), 0 skipped; clippy -D warnings rc 0.