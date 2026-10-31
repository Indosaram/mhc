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

## Delta (omo v5.1.52, 2026-10-30)

Scope: the 28 census rows routed `MIT-cleanroom+SUL` with target `MIT: crates/maho-telemetry`
(`omo-components-34`, `omo-telemetry-01` … `omo-telemetry-27`). This delta was written from the
census row descriptions, the current `crates/maho-telemetry` sources and this spec, plus the
public OpenTelemetry semantic conventions; no SUL source file was opened. Most rows change the
OmO analytics product (vendor transport, anonymous ids, crash reports, on-disk spools, install
notices), which the MIT design does not have (see "What is recorded and where it goes"); those
rows are N-A and listed in the mapping table. The rows that touch something a user can observe in
the ledger, `mhc usage` or the OTLP payload are specified below.

### D1. Model ids are recorded and exported verbatim (new model families included)

Behaviour: the ledger, `mhc usage` and every OTLP signal carry the model id exactly as it appears
on the assistant message. There is no list of known models, no allowlist and no substitution of
unknown ids by a placeholder such as `custom` or `other`. New model families therefore need no
code change; this section pins that with the model ids added upstream in this release
(interface facts): `claude-fable-5-5`, `k3.1-pro`, `k3.1-pro-highspeed`, `kimi-k3.1-pro`,
`kimi-k3.1-pro-highspeed`.

Inputs/outputs: assistant message `provider`, `model` → ledger `provider`/`model`; chat span name
`chat <model>` and `gen_ai.request.model`; metric attribute `gen_ai.request.model`; run log
`mhc.agent.coverage.models_used`; `mhc usage` `byModel` key and summary line.

Error cases: an empty model string is recorded as empty (the session-scan source in `mhc usage`
still skips entries whose `model` is not a string, as before). Ids are never lowercased, trimmed
or truncated.

Tests:
- Given an assistant message with provider `anthropic` and model `claude-fable-5-5`, when it ends,
  then the ledger line has `"model":"claude-fable-5-5"` and, with export on, the run's trace has a
  span named `chat claude-fable-5-5` whose `gen_ai.request.model` is `claude-fable-5-5`.
- Given ledger records for `kimi-coding`/`k3.1-pro-highspeed` and `moonshotai`/`kimi-k3.1-pro`,
  when `mhc usage --json` runs, then `byModel` has two entries carrying exactly those ids.
- Given a model id never seen before (`zz-experimental-9`), when exported, then no attribute in
  the trace, metrics or log payload contains `custom` or `other` in its place.

### D2. Identity and environment invariants of the payload

Behaviour (restates the design as testable invariants, because upstream removed hostname-derived
and machine-derived values in this release): no output of `maho-telemetry` contains

- a host name or any value derived from it (hash, prefix, salted digest);
- a machine fingerprint or a persistent per-installation identifier (no file such as an install
  id is created by this crate);
- the raw value of `SHELL`, `TERM_PROGRAM` or any other environment variable, except values the
  user supplied for OpenTelemetry (`OTEL_RESOURCE_ATTRIBUTES`, `OTEL_SERVICE_NAME`), which are
  the user's own choice;
- crash or panic reports.

The only identifiers exported are the session id (`gen_ai.conversation.id`), random trace/span
ids, `process.pid` and the per-process `service.instance.id` of D5. `host.name` is not set by
default; a user who wants it adds it through `OTEL_RESOURCE_ATTRIBUTES`.

Error cases: none (static property).

Tests:
- Given export to a capturing endpoint and a host name `build-box-gamma`, when one run completes,
  then neither `build-box-gamma` nor its SHA-256 hex digest occurs anywhere in the three decoded
  payloads.
- Given `SHELL=/opt/custom/bin/zsh` and `TERM_PROGRAM=SomeTerm`, when one run completes, then
  neither value occurs in any payload or in the ledger line.
- Given a fresh agent dir, when a session with export and ledger on starts and ends, then the only
  file the crate created is `<usage dir>/usage.jsonl`.
- Given no `OTEL_RESOURCE_ATTRIBUTES`, when one run is exported, then the resource keys are exactly
  `service.name`, `service.version`, `process.pid`, `service.instance.id` and, where D5 maps the
  platform, `os.type` and `host.arch`.

### D3. Exported input-token counts include cached prompt tokens

Behaviour: OpenTelemetry GenAI conventions define `gen_ai.usage.input_tokens` as all prompt
tokens, cached ones included, with the cached part reported separately. Providers report
uncached `input`, `cacheRead` and `cacheWrite` as separate buckets, so the export now adds them:
exported input = `input + cacheRead + cacheWrite`.

Applies to: chat span `gen_ai.usage.input_tokens`; the `gen_ai.client.token.usage` histogram
point with `gen_ai.token.type=input`; the run log attribute `mhc.agent.usage.input_tokens`
(sum of the exported input over the run's chat calls). Unchanged: the ledger `input` field and
every `mhc usage` figure (they keep uncached `input`, so `cacheRate` and `cacheSavings` stay as
specified above), and the separate `cache_read`/`cache_creation` attributes and
`cache_read_input`/`cache_write_input` metric points.

Error cases: the sum saturates at the integer maximum instead of overflowing; a sum of 0 still
skips the metric point (zero values are skipped as before).

Tests:
- Given a chat with `input=100`, `cacheRead=900`, `cacheWrite=50`, when exported, then the span
  has `gen_ai.usage.input_tokens=1050`, `gen_ai.usage.cache_read.input_tokens=900`,
  `gen_ai.usage.cache_creation.input_tokens=50`, and the ledger line still has `"input":100`.
- Given two chats in a run with exported inputs 1050 and 10, then the run log has
  `mhc.agent.usage.input_tokens=1060` and the `input` histogram has points 1050 and 10.
- Given a chat with all three buckets 0, then no `input` histogram point is emitted.

### D4. `gen_ai.provider.name` uses the OpenTelemetry well-known values

Behaviour: for the exported `gen_ai.provider.name` (chat spans and metric points) the
OpenTelemetry conventions require their listed value whenever one applies and allow a custom value
otherwise. mhc sends the listed value for the provider ids in the table below and sends the mhc
provider id itself as the custom value for every other provider. The original mhc provider id is
always exported as well, as attribute `mhc.provider.id` on chat spans and metric points, so no
information is lost. The ledger and `mhc usage` keep the mhc provider id only.

Mapping (interface facts; left = mhc provider id, right = well-known value):

| mhc provider id | `gen_ai.provider.name` |
| --- | --- |
| `anthropic` | `anthropic` |
| `openai`, `chatgpt-subscription`, `openai-codex` (legacy id) | `openai` |
| `azure-openai-responses` | `azure.ai.openai` |
| `amazon-bedrock` | `aws.bedrock` |
| `google` | `gcp.gemini` |
| `google-vertex` | `gcp.vertex_ai` |
| `mistral` | `mistral_ai` |
| `xai` | `x_ai` |
| `groq` | `groq` |
| `deepseek` | `deepseek` |

Any other id (for example `openrouter`, `kimi-coding`, `moonshotai`, a user-defined provider) is
exported unchanged in both attributes. Matching is exact and case-sensitive.

Error cases: an empty provider id exports an empty `gen_ai.provider.name` and an empty
`mhc.provider.id`.

Tests:
- Given a chat from `amazon-bedrock`, when exported, then the span has
  `gen_ai.provider.name=aws.bedrock` and `mhc.provider.id=amazon-bedrock`, and the token metric
  points carry the same two attributes.
- Given a chat from `kimi-coding`, then both attributes are `kimi-coding`.
- Given a ledger written for a `google-vertex` chat, then its `provider` field is
  `google-vertex`.

### D5. Standard resource attributes

Behaviour: the export resource additionally carries these OpenTelemetry semantic-convention
attributes, set before `OTEL_RESOURCE_ATTRIBUTES` is applied so the user can override any of them:

- `service.instance.id`: a random UUID v4 generated once per process (not persisted, not derived
  from the machine);
- `os.type`: from the build target OS: `linux`, `macos` → `darwin`, `windows`, `freebsd`,
  `netbsd`, `openbsd`, `dragonfly` → `dragonflybsd`, `solaris`, `aix`; any other OS → attribute
  omitted;
- `host.arch`: from the build target architecture: `x86_64` → `amd64`, `aarch64` → `arm64`,
  `x86` → `x86`, `arm` → `arm32`, `powerpc` → `ppc32`, `powerpc64` → `ppc64`, `s390x` → `s390x`;
  any other architecture → attribute omitted.

Precedence (lowest to highest): built-in defaults (`service.name=mhc`, the three above) →
`OTEL_RESOURCE_ATTRIBUTES` → `OTEL_SERVICE_NAME` (for `service.name`) → `service.version` and
`process.pid`, which stay authoritative as before. No `os.version`, `host.name` or
product-specific attributes are added (see D2). `mhc usage --status` prints the resolved resource
attributes with their values.

Error cases: none; omission is the fallback for unmapped platforms.

Tests:
- Given a Linux x86_64 build and no `OTEL_RESOURCE_ATTRIBUTES`, when resolved, then the resource
  contains `os.type=linux`, `host.arch=amd64` and a `service.instance.id` that parses as a UUID.
- Given `OTEL_RESOURCE_ATTRIBUTES=os.type=custom-os,service.instance.id=fixed`, then the resource
  has `os.type=custom-os` and `service.instance.id=fixed`.
- Given two resolutions in two separate processes, then their `service.instance.id` values
  differ; given two resolutions in the same process, they are equal.

### D6. Export opt-out is re-checked when data is pushed

Behaviour: the switches are no longer checked only at startup. Immediately before each push (at
run end and at the shutdown flush) the extension reads the current merged configuration view and
evaluates `telemetry.enabled` and `telemetry.otlpExportEnabled` again. If either is now `false`,
the pending payloads for all signals are discarded unsent. Environment variables are not re-read
(they cannot change inside the process). The re-check can only turn export off: a session that
started with export off never starts exporting, even if the configuration later allows it. The
ledger is unaffected by this section.

Outputs: on the first discard in a session, one diagnostic line that says export was skipped
because it was turned off in the configuration and gives the number of discarded payloads; later
discards in the same session add no further lines. `mhc usage --status` reflects the current
configuration.

Error cases: if the host returns no configuration view at push time (an error, not a config file
that fails to parse: in that case the host keeps the previous view, config.md section 14),
the push is treated as not allowed (fail closed) and reported through the same once-per-session
diagnostic.

Tests:
- Given export on at startup and a capturing endpoint, when `telemetry.otlpExportEnabled` is set
  to `false` in the project config during the session and a run then ends, then the endpoint
  receives no request and exactly one diagnostic line naming the discarded payload count is
  emitted.
- Given the same session, when a second run ends, then still no request is sent and no second
  diagnostic appears.
- Given export off at startup because no endpoint is configured, when the config later sets
  `telemetry.enabled: true`, then no request is ever sent.
- Given a config view whose read fails at push time, then nothing is sent and the diagnostic is
  emitted.
- Given the ledger on and export turned off mid-session, then ledger lines keep being written.

### Mapping

| census row | delta § | APPLIES/N-A | reason |
| --- | --- | --- | --- |
| omo-components-34 | D1, D2 | APPLIES | new model ids are recorded/exported verbatim (D1); `machine_hash` never exists (D2); computer-use result kinds are not telemetry fields in the MIT design |
| omo-telemetry-01 | D2 | N-A | N-A reason: the MIT design has no persistent installation id by intent; D2 forbids creating one |
| omo-telemetry-02 | D2 | APPLIES | no hostname-derived identifier in any output (D2 invariant and test) |
| omo-telemetry-03 | - | N-A | N-A reason: the legacy daily-active product and its id prefix do not exist in the MIT crate |
| omo-telemetry-04 | D2 | APPLIES | no `machine_hash`-like resource or event field (D2 invariant and test) |
| omo-telemetry-05 | D2 | N-A | N-A reason: the MIT payload has no shell property; D2 forbids raw `SHELL` |
| omo-telemetry-06 | D2 | N-A | N-A reason: the MIT payload has no terminal property; D2 forbids raw `TERM_PROGRAM` |
| omo-telemetry-07 | - | N-A | N-A reason: export is opt-in by configuring an endpoint; no installer disclosure text exists (SD-6) |
| omo-telemetry-08 | - | N-A | N-A reason: no first-run telemetry notice in the MIT design (SD-6) |
| omo-telemetry-09 | D2 | N-A | N-A reason: the MIT crate never sends crash reports; D2 states it |
| omo-telemetry-10 | - | N-A | N-A reason: no ephemeral-home consent rule; export depends only on `OTEL_*`/opt-out switches |
| omo-telemetry-11 | - | N-A | N-A reason: config switches come from the `maho-config` merged view, which owns profile/home resolution; no separate consent re-evaluation record |
| omo-telemetry-12 | D1 | APPLIES | model ids including `claude-fable-5-5` and the Kimi K3.1 ids pass through verbatim; no vocabulary list to extend |
| omo-telemetry-13 | - | N-A | N-A reason: user-observable, but not telemetry: the validated `OMO_BUNDLED_SKILLS_DIR` override is specified in skills.md Delta D1/D2 (`maho-skills`) |
| omo-telemetry-14 | D1 | N-A | N-A reason: export goes to the user's own collector; no pinned model list or `custom` masking (D1 forbids it) |
| omo-telemetry-15 | D3, D4 | APPLIES | cache-inclusive `gen_ai.usage.input_tokens` (D3) and well-known `gen_ai.provider.name` (D4); the product-event projection, pinned enum values and cost fixed-point field are N-A (no product events) |
| omo-telemetry-16 | - | N-A | N-A reason: no built-in ingest endpoint or key; export targets only user-configured `OTEL_*` endpoints |
| omo-telemetry-17 | D5 | APPLIES | standard resource attributes `service.instance.id`, `os.type`, `host.arch`; product-specific resource keys, spool path and timeouts are N-A |
| omo-telemetry-18 | - | N-A | N-A reason: single export destination; no dual-send wrapper or lazy transport loading |
| omo-telemetry-19 | D6 | APPLIES | opt-out is re-checked at push time and a turned-off export drops pending data with one diagnostic |
| omo-telemetry-20 | - | N-A | N-A reason: one producer (the telemetry extension); wiring of several product producers does not exist |
| omo-telemetry-21 | - | N-A | N-A reason: no on-disk OTLP spool; pending data lives in memory and is dropped on exit |
| omo-telemetry-22 | - | N-A | N-A reason: no on-disk OTLP spool, so no writer owner markers |
| omo-telemetry-23 | - | N-A | N-A reason: no summary queue or spool to purge on opt-out |
| omo-telemetry-24 | - | N-A | N-A reason: type-only upstream change, no behaviour |
| omo-telemetry-25 | - | N-A | N-A reason: JS packaging only |
| omo-telemetry-26 | - | N-A | N-A reason: generated collector schema artifact of the OmO product; vocabulary parts covered by D1/D2 |
| omo-telemetry-27 | - | N-A | N-A reason: upstream docs only |

Leak self-check:

- [x] no fenced code block is Rust, TypeScript or JS from the SUL tree (this delta has no code
      blocks);
- [x] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt (written
      from census row summaries, the MIT crate and OpenTelemetry semantic conventions; no SUL
      source opened);
- [x] no list or table mirrors a SUL constant table row-for-row: the model ids in D1 and the
      provider/OS/arch mappings in D4/D5 are interface facts (model ids, OpenTelemetry well-known
      values); the upstream terminal-program list, pinned model list, event-field map and event
      allowlist are not reproduced;
- [x] no proposed identifier is an OmO-original name (new keys: `mhc.provider.id`; the rest are
      OpenTelemetry names);
- [x] OmO-original features are absent (no analytics vendor, install id, crash capture, notices,
      spools or product events).
