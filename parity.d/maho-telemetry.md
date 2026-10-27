# maho-telemetry (MIT migration Phase 2, step 5: telemetry)

| Source (MIT) | Commit | Rust |
|---|---|---|
| can1357/oh-my-pi `packages/coding-agent/src/telemetry-export.ts`, `telemetry-settings.ts` | f59bbac | crates/maho-telemetry/src/settings.rs |
| oh-my-pi `packages/coding-agent/src/telemetry-export-otlp.ts` | f59bbac | crates/maho-telemetry/src/otlp.rs (metrics, run log, resource) |
| oh-my-pi `packages/agent/src/telemetry.ts` (span tree, `gen_ai.*` keys) | f59bbac | crates/maho-telemetry/src/otlp.rs (traces) |
| oh-my-pi `packages/stats/src/parser.ts` | f59bbac | crates/maho-telemetry/src/record.rs |
| oh-my-pi `packages/stats/src/{rollup,index}.ts`, `client/data/formatters.ts`, `packages/utils/src/format.ts` | f59bbac | crates/maho-telemetry/src/stats.rs |
| oh-my-pi `packages/coding-agent/src/cli/stats-cli.ts` | f59bbac | crates/maho-telemetry/src/cli.rs, `mhc usage` |
| clean-room spec docs/cleanroom/specs/telemetry.md | — | opt-outs, ledger, `mhc usage` surface, `src/proto.rs` |

Differences from the originals:

- Opt-outs added: `MAHO_TELEMETRY` (everything), `DO_NOT_TRACK` (export), `MAHO_USAGE_STATS`
  and `telemetry.usageStats`/`telemetry.enabled` (config). oh-my-pi reads only
  `telemetry.otlpExportEnabled` and `OTEL_SDK_DISABLED`.
- Local statistics come from an append-only ledger written per model call (works for
  `--no-session` runs) instead of re-ingesting session logs into SQLite; `mhc usage --sessions`
  still reads session logs. No dashboard server.
- Cache savings derive the uncached input rate from the recorded `cost.input / input` instead of
  the model catalogue; premium requests and service tiers are not tracked.
- The OTel SDK is replaced by a hand-written OTLP protobuf encoder and one push per agent run
  (also at session shutdown, ≤ 3 s); no batching window, no 30 s timer. Only `http/protobuf`, as
  in the original. The common endpoint gets `/v1/<signal>` appended (OTLP/HTTP spec).
- Span/metric/log names keep the OpenTelemetry GenAI keys; the vendor prefix `omp.` is `mhc.`.
  Spans carry token counts, finish reason, TTFT and cost; no request parameters, tool
  definitions or content. Run coverage lists only models used and tools invoked.
- `service.name` defaults to `mhc`.
