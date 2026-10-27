# Spec: telemetry export and local usage statistics (MIT profile)

Status: written 2026-10-28 for Phase 2 step 5 (telemetry). Sources are MIT sources and public
conventions only; no SUL source (`crates/omo/**`, including `telemetry-core` and
`maho-omo-telemetry`, the omo telemetry extension, or any OmO/COMP file) was opened. The OmO
telemetry product (its event names, its analytics vendor, its anonymous-id scheme) is
OmO-original and out of scope (plan §0); nothing here is modelled on it.

| Source (license verified MIT) | Commit | Used for |
| --- | --- | --- |
| `can1357/oh-my-pi` `packages/coding-agent/src/telemetry-export.ts`, `telemetry-settings.ts`, `telemetry-export-otlp.ts` | `f59bbac3d56e8b485fef466388b1cb5be4b93123` | export gate (`telemetry.otlpExportEnabled`, `OTEL_SDK_DISABLED`), per-signal endpoint/exporter/protocol resolution, resource, metric/log names |
| oh-my-pi `packages/agent/src/telemetry.ts` | same | GenAI span tree (`invoke_agent`, `chat {model}`, `execute_tool {tool}`) and attribute keys |
| oh-my-pi `packages/stats/src/{parser,rollup,index}.ts`, `client/data/formatters.ts`, `packages/utils/src/format.ts` | same | usage record extraction from session JSONL, aggregate formulas, summary text and number formats |
| oh-my-pi `docs/environment-variables.md` §11 | same | user-facing `OTEL_*` contract |
| OpenTelemetry specification (OTLP/HTTP, SDK environment variables) | public spec | endpoint path rules, `http/protobuf` wire format, header list syntax |
| Console Do Not Track convention (`DO_NOT_TRACK=1`) | public convention | the extra opt-out variable |

oh-my-pi has no local usage *ledger* (its `omp stats` re-reads session logs into SQLite) and does
not read `DO_NOT_TRACK`; those two parts are specified here and implemented clean-room. Everything
else is a port.

## What is recorded and where it goes

Two independent outputs, each with its own switch:

1. **Local usage ledger** — one JSON line per completed model call, written on this machine only:
   `<usage dir>/usage.jsonl`, where `<usage dir>` is `$MAHO_USAGE_DIR` if set and non-empty, else
   `<agent dir>/stats`. Never sent anywhere. No prompt, response, tool argument or file content is
   written; only counters and identifiers listed below.
2. **OTLP export** — traces, metrics and logs pushed to an OpenTelemetry collector the user
   configured with `OTEL_*` variables. Nothing is exported unless an endpoint is configured; there
   is no built-in endpoint, no vendor, and no anonymous installation id.

## Opt-out (resolution order)

`resolve(env, config)` returns `{ export: Option<SignalConfig>, ledger: bool, reasons: [..] }`.

A variable is *truthy* when its trimmed, lowercased value is non-empty and not one of `0`, `false`,
`no`, `off`. It is *falsy* when the value is one of `0`, `false`, `no`, `off`.

Master switch: `MAHO_TELEMETRY` falsy, or config `telemetry.enabled: false` → both outputs off.

Export is off when any of these holds (each adds a reason string):

- master switch off;
- `DO_NOT_TRACK` truthy (any value other than empty/`0`/`false`/`no`/`off`, so `DO_NOT_TRACK=1`
  and `DO_NOT_TRACK=true` both opt out);
- config `telemetry.otlpExportEnabled: false` (oh-my-pi key, default `true`);
- `OTEL_SDK_DISABLED` equals `true` (case-insensitive, trimmed; oh-my-pi rule);
- no signal is enabled by the per-signal rules below.

The ledger is off when the master switch is off, `MAHO_USAGE_STATS` is falsy, or config
`telemetry.usageStats: false` (default `true`). `DO_NOT_TRACK` does **not** turn the ledger off:
the ledger never leaves the machine and is the data `mhc usage` reports. (Users who want no local
file set `MAHO_USAGE_STATS=0` or `MAHO_TELEMETRY=0`.)

Config keys are read from the merged native configuration view (`maho-config`) at the JSON
pointers `/telemetry/enabled`, `/telemetry/otlpExportEnabled`, `/telemetry/usageStats`; a
non-boolean value is ignored (default applies).

## Per-signal rules (port of oh-my-pi `resolveSignalConfig`/`signalEnabled`)

For each signal `S` in traces, metrics, logs:

- endpoint = `OTEL_EXPORTER_OTLP_<S>_ENDPOINT`, else `OTEL_EXPORTER_OTLP_ENDPOINT`;
- exporter selection = `OTEL_<S>_EXPORTER` split on `,`, trimmed, lowercased, empty entries
  dropped: any `none` → off; a non-empty selection without `otlp` → off;
- no endpoint → off;
- protocol = `OTEL_EXPORTER_OTLP_<S>_PROTOCOL`, else `OTEL_EXPORTER_OTLP_PROTOCOL`; a value other
  than `http/protobuf` → off (with a one-line diagnostic naming the unsupported protocol).

Request URL (OTLP/HTTP spec): a per-signal endpoint is used as given; the common endpoint gets
`/v1/traces`, `/v1/metrics` or `/v1/logs` appended (after removing one trailing `/`).

Headers: `OTEL_EXPORTER_OTLP_HEADERS`, then `OTEL_EXPORTER_OTLP_<S>_HEADERS` merged per key over
it. Format `k=v,k2=v2`; keys and values are trimmed and percent-decoded; entries without `=` are
skipped. Header values are secrets: never print them (diagnostics name only the variable).

Resource: `service.name` = `mhc`, overridden by `OTEL_RESOURCE_ATTRIBUTES` (`k=v,...`,
percent-decoded), and `service.name` finally overridden by `OTEL_SERVICE_NAME`. Also
`service.version` = the mhc version and `process.pid`.

## Ledger record (`usage.jsonl`, one object per line)

Written on every `message_end` whose message is an assistant message, in append mode, one
`write` per line. Field names (camelCase):

| field | value |
| --- | --- |
| `v` | `1` |
| `timestamp` | message `timestamp` (ms since epoch); 0/absent → time of writing |
| `sessionId` | session id from the session manager (empty when none) |
| `cwd` | the session working directory |
| `agent` | `main`, or `subagent` when `$MHC_TASK_DEPTH` is a positive integer |
| `provider`, `model`, `api` | from the message |
| `stopReason` | message `stopReason`; absent → `error` if `errorMessage` is set, else `aborted` |
| `input`, `output`, `cacheRead`, `cacheWrite`, `reasoning` | token counters (non-finite/non-numeric → 0) |
| `totalTokens` | provider total when finite, else the sum of the four buckets |
| `cost` | `{input, output, cacheRead, cacheWrite, total}` USD as recorded on the message; omitted when absent |
| `durationMs` | `timing.endedAt - timing.requestedAt` when both are present and the result ≥ 0 |
| `ttftMs` | `timing.firstTokenAt - timing.requestedAt` when present and ≥ 0 |

A write failure is reported once per session as a diagnostic and never fails the turn.

## `mhc usage`

```
mhc usage [--json] [--sessions] [--days N] [--file PATH]
```

- Source: the ledger by default; `--sessions` instead scans every `*.jsonl` under the sessions
  directory (`<agent dir>/sessions`, recursively). A session file's records take `cwd` from its
  `type: "session"` header (else the decoded directory name) and `agent` = `subagent` when the file
  is nested more than one directory below the sessions directory (oh-my-pi
  `classifyAgentType`). Only `type: "message"` entries with a non-empty `id` and
  `message.role == "assistant"` and string `model`/`provider`/`api` and an object `usage` count
  (oh-my-pi `extractStats`). Unparseable lines are skipped.
- `--days N` keeps records with `timestamp >= now - N days`. `--file` reads another ledger.
- Output: the summary text below, or with `--json` an object
  `{source, records, overall, byModel, byFolder, byDay}`. `--sessions` never writes the ledger.
- Exit 0 with "No usage recorded yet." when there is nothing to report.

Aggregates (oh-my-pi `toAggregatedStats`), over a set of records:

- `totalRequests`, `failedRequests` (`stopReason == "error"`), `successfulRequests`,
  `errorRate = failed / total` (0 when no requests);
- token totals per bucket; `cacheRate = cacheRead / (input + cacheRead)` (0 when the denominator is
  0);
- `cacheSavings`: per record with `input > 0` and a recorded `cost.input > 0`, the uncached prompt
  cost is `(cost.input / input) * (input + cacheRead + cacheWrite)` and the actual prompt cost is
  `cost.input + cost.cacheRead + cost.cacheWrite`; `cacheSavings = (Σuncached − Σactual) / Σuncached`
  (0 when `Σuncached` is 0). (oh-my-pi prices the uncached prompt from its model catalog; mhc
  derives the input rate from the recorded cost.)
- `totalCost = Σ cost.total`; `unpricedRequests` = records with `totalTokens > 0` and no `cost` or
  `cost.total == 0`;
- `avgDurationMs`, `avgTtftMs`: mean over records that have the field, else null;
  `avgTokensPerSecond`: mean of `output * 1000 / durationMs` over records with `durationMs > 0`;
- `firstTimestamp`, `lastTimestamp`.

Groupings: `byModel` (key provider + model), `byFolder` (cwd), each sorted by `totalCost`
descending then `totalRequests` descending; `byDay` (UTC day start, ascending) with `requests`,
`errors`, `tokens` (`totalTokens`), `cost`.

Summary text (port of `printStatsSummary`, without colour):

```
=== AI Usage Statistics ===

Overall:
  Requests: <n> (<failed> errors)
  Error Rate: <errorRate>
  Total Tokens: <input+output>
  Input Tokens: <n>
  Output Tokens: <n>
  Cache Rate: <pct>
  Cache Savings: <pct>
  API-equivalent estimate: <cost>
  Avg Duration: <dur or ->
  Avg TTFT: <dur or ->
  Avg Tokens/s: <x.y>            (only when known)

By Model (API-equivalent estimates):
  <model>: <n> reqs, <cost>, <pct> cache rate, <pct> cache savings     (top 10)

By Folder (API-equivalent estimates):
  <folder>: <n> reqs, <cost>                                           (top 10)
```

Formats (oh-my-pi utils): numbers `formatNumber` (`999`, `1.5K`, `25K`, `1.5M`, `25M`, `1.5B`);
percent `x.y%`; error rate `<0.01%` below 0.005 %, two decimals below 0.1 %; cost `N/A` when 0
with unpriced requests, `$x.xxxx` below 0.01, `$x.xxx` below 1, else `$x.xx`; durations
`formatDuration` (`123ms`, `1.5s`, `30m15s`, `2h30m`, `3d2h`).

## OTLP export (port of oh-my-pi telemetry export, reduced)

Collected per agent run (`agent_start` … `agent_end`) and pushed when the run ends; whatever is
still buffered is pushed at session shutdown, waiting at most 1.5 s. Pushing happens on a
background thread with a 10 s request timeout; failures are reported once as a diagnostic and
never affect the session. Content capture is always off: no prompt, message, tool argument or
result text is exported.

Traces (`/v1/traces`), one trace per run:

- root span `invoke_agent mhc` (kind INTERNAL), attributes `gen_ai.operation.name=invoke_agent`,
  `gen_ai.agent.name=mhc`, `gen_ai.conversation.id=<session id>`, `mhc.agent.step_count`;
- one `chat <model>` span (kind CLIENT) per assistant message: start/end from `timing`
  (`requestedAt`/`endedAt`, else the message timestamp), attributes `gen_ai.operation.name=chat`,
  `gen_ai.provider.name`, `gen_ai.request.model`, `gen_ai.response.finish_reasons` (array with the
  stop reason), `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`,
  `gen_ai.usage.cache_read.input_tokens`, `gen_ai.usage.cache_creation.input_tokens`,
  `gen_ai.usage.reasoning.output_tokens` (when > 0), `gen_ai.response.time_to_first_chunk`
  (seconds, when known), `mhc.gen_ai.cost.usd` (when recorded); status ERROR when the stop reason
  is `error`;
- one `execute_tool <tool>` span (kind INTERNAL) per tool execution, attributes
  `gen_ai.operation.name=execute_tool`, `gen_ai.tool.name`, `gen_ai.tool.call.id`; status ERROR
  when the tool reported an error.

Metrics (`/v1/metrics`, delta temporality, one export per run):

- histogram `gen_ai.client.token.usage` (unit `{token}`), one point per chat call and token type
  (`input`, `output`, `total`, `cache_read_input`, `cache_write_input`, `reasoning_output`; zero
  values skipped), attributes `gen_ai.operation.name`, `gen_ai.provider.name`,
  `gen_ai.request.model`, `gen_ai.token.type`;
- counter `mhc.agent.chat.cost.estimated_usd` (unit `USD`, double sum) per provider/model when > 0;
- counter `mhc.agent.runs` (unit `{run}`), counter `mhc.agent.chat.calls` per
  `gen_ai.response.finish_reason`, counter `mhc.agent.tool.calls` per `gen_ai.tool.name` and
  `mhc.tool.status` (`ok`/`error`), counter `mhc.agent.errors` per `error.type` (`chat`, `tool`).

Logs (`/v1/logs`): one record per run, severity INFO, body `agent run completed`, event name
`mhc.agent.run.completed`, attributes `mhc.agent.step_count`, `mhc.agent.chats.total`,
`mhc.agent.tools.total`, `mhc.agent.tools.error`, `mhc.agent.usage.input_tokens`,
`mhc.agent.usage.output_tokens`, `mhc.agent.usage.total_tokens`, `mhc.agent.cost.estimated_usd`,
`mhc.agent.coverage.models_used`, `mhc.agent.coverage.tools_invoked` (comma lists). `OTEL_LOG_LEVEL`
`none`/`error`/`warn` suppresses it (it is an INFO record).

Wire format: OTLP protobuf (`ExportTraceServiceRequest`, `ExportMetricsServiceRequest`,
`ExportLogsServiceRequest`) POSTed with `Content-Type: application/x-protobuf`; one
`ResourceSpans`/`ResourceMetrics`/`ResourceLogs` with the resource above and one scope
`maho-telemetry` + crate version. Trace ids are 16 random bytes, span ids 8 random bytes.

## Surfaces

- `<builtin:telemetry>` extension, mounted in the MIT profile only; no tools, no prompt text.
- `/usage` slash command: the summary for the ledger.
- `mhc usage ...` from the shell.
- `mhc doctor` is unchanged; `mhc usage --status` prints the resolved settings (ledger path,
  ledger on/off, export on/off with reasons and the enabled signals and URLs, never headers).

## Not covered

oh-my-pi's stats dashboard server, SQLite rollups, frustration/tool/gain metrics, premium request
accounting, model price catalogue, run-collector coverage of unused tools, content capture modes,
handoff/judgment spans, log forwarding of the application logger, periodic 30 s flush, gRPC and
`http/json` protocols.

## Leak self-check

- [x] no SUL file was opened while writing this spec;
- [x] no OmO-original telemetry concept (analytics vendor, anonymous id, OmO event names) appears;
- [x] identifiers are neutral (`maho-telemetry`, `mhc usage`, `mhc.*` attribute prefix) or
      OpenTelemetry semantic-convention names.
