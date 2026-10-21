# CI10 classifier catalog repair

Source repair complete; runtime verification was not executed.

## Exact failure

Evidence: `/home/projects/mhc/.omo/nonwindows-20261020/ci-39437518025/test__1_.log`, lines 985-1027.

`models::classifier_tests::mixed_catalog_parses_classifier_without_chat_fields` failed at `crates/maho-ai/src/api/classifier_tests.rs:230`: actual classifier count 41, expected 14. Deserialization and the preceding `ModelType::Classifier` assertion had succeeded. This was not a metadata parse or cost calculation failure.

The shard reported 4719 tests run, 4718 passed, 1 failed, 1 skipped, exit 100. No other runtime failure appears in this shard summary.

## Repair and pinned evidence

Reference: senpi `903b83367545bea516f35fc7f7c7cb692847988d` at `/tmp/parity-up/senpi-1019`.

Only `src/api/classifier_tests.rs` changed for this repair:

- Updated the exact catalog cardinality assertion from 14 to 41: typesafe 1, cloudflare-workers-ai 1, vercel-ai-gateway 12, openrouter 27. The added 27 rows come from pinned `packages/ai/src/providers/data/openrouter.json`; reducing or dropping these valid rows would break upstream parity.
- Strengthened the same test to serialize and deserialize every catalog classifier through `AnyModel`, asserting full model equality. This covers all metadata and cost fields, not just the original free Jev model.
- Explicitly asserts that every serialized classifier lacks chat-only `reasoning` and `maxTokens` fields.
- Preserved the original Jev classifier-type assertion and rejection of an unknown model type. No test was deleted, skipped, or changed to a loose nonempty/lower-bound assertion.

Pinned `packages/ai/src/types.ts:1404-1408` defines ClassifierModel as BaseModel plus type and contextWindow, without chat-only reasoning/maxTokens. Pinned `test/classifier-models.test.ts` checks typed catalog separation and iterates every OpenRouter classifier for the System One API and base URL. It does not impose the obsolete 14-entry catalog cap. Pinned `src/providers/openrouter.ts` includes the classifier catalog and registers typesafe-system-one.

## Related-assumption inventory

Source reads covered native `src/types.rs`, `src/models_generated.rs`, `src/models.rs` provider construction, `src/api/classifier_tests.rs`, `src/api/system_one_shared.rs`, `src/api/typesafe_system_one.rs`, and catalog consumers found by CLASSIFIER_MODELS search. The old 14-entry assertion was the only catalog-size assumption found among these consumers.

- **Prices are not universally zero.** Restored OpenRouter rows contain input prices from 0 through 0.15 per million tokens. Free entries coexist with paid entries. The catalog remains unchanged; no cost was normalized to zero to make a test pass.
- **Cache rates are metadata, even when this API reports no cache tokens.** liquid/d1, liquid/d1-ultra, upstage/solar-decide, and upstage/solar-decide-flash carry nonzero cacheRead rates. Native ClassifierModel keeps the complete ModelCost, and the expanded round-trip assertion preserves it.
- **Usage semantics match pinned upstream.** Native `system_one_shared.rs:271-284` reads input_tokens/output_tokens, leaves cache token counts at zero, and delegates pricing to calculate_cost using a request model that clones the classifier's full cost card. Pinned `src/api/system-one-shared.ts:127-149` explicitly does the same. No missing cache accounting repair is justified by the catalog's nonzero cache rates alone. Existing paid-usage test still asserts 0.02 for one million liquid/d1 input tokens; it was not changed.
- **Classifier metadata is heterogeneous.** The 27 OpenRouter rows include text-only and text/image input, context windows from 4096 to 1050000, namespaced IDs, leading-tilde aliases, and colon-suffixed free IDs. Native ClassifierModel stores input, ID, context window, and full pricing without assuming chat fields. The expanded round-trip assertion includes all rows.
- **Routing is API-based, not model-name-based.** OpenRouter classifiers use typesafe-system-one and `https://openrouter.ai/api/v1`; the native transport appends `/systemone`, matching upstream. Provider construction iterates catalog entries dynamically. No fixed classifier count or zero-price requirement exists in that path.

No production parser, catalog row, provider route, or pricing behavior needed modification for this CI10 failure.

## Execution boundary

All edits stayed within `/home/projects/mhc-wt/nonwindows-20261020/crates/maho-ai/**` and used apply_patch. No cargo, tests, CI, commits, lint, or explicit diagnostics were run. The source was investigated directly; the updated Rust test remains uncompiled and unexecuted under the user's no-run instruction. Parent owns the next global verification gate.
