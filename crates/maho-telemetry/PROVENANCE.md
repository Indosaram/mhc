# Provenance — maho-telemetry

- License: MIT.
- Method: **P** (port of MIT originals) for the export gate and `OTEL_*` per-signal rules, the
  GenAI span tree and attribute names, the run metrics and run log, usage extraction from session
  logs, the aggregate formulas and the summary/number formats; **C** (from
  `docs/cleanroom/specs/telemetry.md`, itself written from the MIT sources below and public
  OpenTelemetry / Do-Not-Track conventions) for the `MAHO_TELEMETRY`/`DO_NOT_TRACK`/
  `MAHO_USAGE_STATS` opt-outs, the local usage ledger, the `mhc usage` command shape and the
  hand-written OTLP protobuf encoder.
- Origin (LICENSE checked as MIT before reading; checkout in `/workspace/work/upstream/oh-my-pi`):
  `can1357/oh-my-pi` @ `f59bbac3d56e8b485fef466388b1cb5be4b93123` (root `LICENSE` MIT, Copyright
  Mario Zechner, Can Bölük, Stencil Labs; `packages/stats/LICENSE` MIT, Copyright Can Bölük):
  - `packages/coding-agent/src/telemetry-export.ts` (`initTelemetryExport` gate,
    `resolveSignalConfig`, `signalEnabled`) and `telemetry-settings.ts`
    (`telemetry.otlpExportEnabled`) → `src/settings.rs`;
  - `packages/coding-agent/src/telemetry-export-otlp.ts` (`AgentMetricRecorder` metric names,
    units and attributes, `emitRunSummaryLog`, `parseOtelLogLevel`, resource with
    `OTEL_SERVICE_NAME`/`OTEL_RESOURCE_ATTRIBUTES`) → `src/otlp.rs` (`omp.` prefix → `mhc.`);
  - `packages/agent/src/telemetry.ts` (span hierarchy `invoke_agent` / `chat {model}` /
    `execute_tool {tool}`, `gen_ai.*` attribute keys) → `src/otlp.rs`;
  - `packages/stats/src/parser.ts` (`extractStats`, `resolveUsageTotal`, `finiteTokenCount`,
    `classifyAgentType`, `extractFolderFromPath`) → `src/record.rs`;
  - `packages/stats/src/rollup.ts` (`toAggregatedStats`), `index.ts` (`printStatsSummary`,
    `formatCost`), `client/data/formatters.ts` (`formatErrorRate`), `packages/utils/src/format.ts`
    (`formatNumber`, `formatDuration`, `formatPercent`) → `src/stats.rs`;
  - `packages/coding-agent/src/cli/stats-cli.ts` (`--summary`/`--json` one-shot report) →
    `src/cli.rs`.
- Not opened: any SUL source (`crates/omo/**`, including `telemetry-core` and
  `maho-omo-telemetry`), the omo telemetry extension, OmO/COMP sources, and lazycodex's
  relicensed telemetry component (the OmO telemetry product is out of scope per plan §0).
- Not ported: the stats dashboard server and SQLite rollups, frustration/gain/tool dashboards,
  premium-request accounting, the model price catalogue, the OpenTelemetry SDK itself (replaced by
  `src/proto.rs` + reqwest), content capture, handoff/judgment spans, application-log forwarding,
  periodic 30 s flushing, gRPC and `http/json`.
