# X search — behavior spec (Phase 4, unit `x-search`)

Scope: an agent tool that searches posts on X (Twitter) through xAI's server-side X search, plus
the optional usage skill shipped with it. Replaces `components/maho-omo-x-search` (SUL). The
wire format is xAI's public Responses API; everything else here is observable behavior. Proposed
crate name: `maho-x-search`; extension mount name `x-search`.

## 1. Availability (load time)

The tool exists only when an xAI credential is connected. The check runs once, synchronously,
when the extension registers:

1. `<agent dir>/auth.json` exists → connected iff it parses as a JSON object with an `xai` entry
   that is an object whose `type` is `"oauth"` or `"api_key"`. Unreadable file, invalid JSON, a
   missing `xai` entry or any other `type` → not connected (fail closed; the env var is **not**
   consulted in this case).
2. `auth.json` absent → connected iff `XAI_API_KEY` is set and non-blank after trimming.

Not connected → nothing is registered (no tool, no skill) and nothing is printed at normal log
levels; at most a debug-level line. Connected → the tool from §2 is registered, and the skill
from §6 is contributed.

## 2. Tool surface (interface facts)

- Name `x_search`; a short human label in the implementer's own words; runs in parallel with
  other tools.
- Exposed through tool search (deferred/lazy activation) rather than always-on, in a search group
  `x-search`. Search keywords are the implementer's choice; a tool search for X posts, tweets or
  Twitter must find the tool.
- The description is newly written; it must say that the tool searches X posts through xAI and
  needs a connected xAI credential. Parameter details belong in the schema, not copied prose.

Parameters (JSON schema, `additionalProperties: false`):

| Key | Type / bounds | Default |
| --- | --- | --- |
| `query` (required) | string, 1..2000 characters (UTF-16 code units), not blank | — |
| `from_date`, `to_date` | string `YYYY-MM-DD`, a real calendar date, inclusive | — |
| `allowed_x_handles`, `excluded_x_handles` | array of unique strings matching `^[A-Za-z0-9_]{1,15}$`, 1..20 items | — |
| `mode` | `"latest"` or `"top"` | `"latest"` |
| `max_results` | integer 1..30 | 10 |
| `enable_image_understanding`, `enable_video_understanding` | boolean | false |

## 3. Validation (in this order; first failure wins)

Each failure is a tool result with `is_error: true` (not a thrown error) and a code:

1. input is not a JSON object → `INVALID_PARAMS`;
2. `query` is a string that is empty after trimming → `INVALID_PARAMS`;
3. both handle lists present → `INVALID_FILTERS`;
4. a handle list longer than 20 → `TOO_MANY_HANDLES` (message names the key);
5. `from_date`/`to_date` present but not a string, not `YYYY-MM-DD` with ASCII digits, or not a
   real date (Feb 29 only in leap years; years below 100 are rejected) → `INVALID_DATE` (names
   the key);
6. both dates present and `from_date` sorts after `to_date` → `INVALID_DATE_RANGE`;
7. any other schema violation (unknown key, wrong type, out-of-range number, bad/duplicate handle,
   empty handle list, query too long, missing query) → `INVALID_PARAMS`.

## 4. Call lifecycle

1. Validate (§3).
2. Resolve the bearer **on every call** (no caching, so a refreshed login is picked up):
   if the host's credential store has an `xai` credential, use the provider auth it resolves
   (trimmed API key; empty or failed resolution → no credential, without falling back to the env
   var); otherwise use trimmed non-empty `XAI_API_KEY`. None → `AUTH` error whose text tells the
   user to log in to xAI (`/login`) or set `XAI_API_KEY`.
3. Choose the model: `grok-4.20-0309-non-reasoning` by default; a non-blank
   `OMO_X_SEARCH_MODEL` env var overrides it (env name kept for compatibility; the implementer may
   also accept a neutral alias such as `MAHO_X_SEARCH_MODEL`, which then takes precedence).
4. `POST https://api.x.ai/v1/responses` with `content-type: application/json` and
   `authorization: Bearer <key>`. Body:
   - `model`; `input`: one `user` message whose wording is newly written. It must contain the
     query text followed by ` since:<from_date>` / ` until:<to_date>` when those are set (X search
     operators), the chosen mode and the `max_results` number;
   - `tools`: one object `{type: "x_search"}` plus `from_date`, `to_date`, `allowed_x_handles`,
     `excluded_x_handles` when given and both `enable_*_understanding` booleans always (also when
     false);
   - `tool_choice: "required"`, `max_turns: 1`, `parallel_tool_calls: false`,
     `max_output_tokens: 4000`, `store: false`; no `reasoning` field for the default model.
5. Deadline 60 s; caller cancellation is honoured immediately (also when already cancelled
   before sending; then no request is made).
6. Outcome mapping:

| Condition | Code |
| --- | --- |
| caller cancelled | `UPSTREAM` (message indicates cancellation) |
| deadline hit | `TIMEOUT` (states the ms) |
| transport error | `UPSTREAM` (carries the transport message) |
| HTTP 401 / 403 | `AUTH` |
| HTTP 429 | `RATE_LIMITED` (the HTTP layer parses a numeric `retry-after`; the tool result carries only the code) |
| other non-2xx | `UPSTREAM` (states the status) |
| 2xx with non-JSON body | `PROTOCOL` |

## 5. Result normalization and output

From the JSON response:

- Results: post URLs matching `http(s)://[www.]x.com/<handle>/status/<digits>` or
  `.../i/status/<digits>` (id ≤ 30 digits), deduplicated by post id, in this order of discovery:
  (a) `url_citation` annotations on `output[type=message].content[].annotations`, (b) post URLs
  found in those message texts, (c) string entries of top-level `citations`. Truncated to
  `max_results`.
- Each result: `id`, `url`, `title` (annotation title unless it is empty, all digits, or a URL —
  then the URL), `snippet` (the first trimmed message-text line that contains the URL or
  `/status/<id>`, else empty).
- Queries: for each `output` item of type `custom_tool_call` or `x_search_call` whose `name` is
  one of `x_keyword_search`, `x_semantic_search`, `x_user_search`, `x_thread_fetch` and whose
  `input` is a JSON-object string, one string `k=v k=v ...` (string values raw, others as JSON).
  Unparseable inputs are skipped.
- Usage: `usage.server_side_tool_usage_details.x_search_calls` and `usage.cost_in_usd_ticks`
  (0 when absent). Malformed shapes anywhere yield empty lists, never an error.

Text content (marked interface fact: the line prefixes below are what prompts and tests match):
a header line `x_search results: <n>`, then per result a numbered block with title, `URL: ...`
and `Snippet: ...`, then `Queries used: ` with the queries joined by ` | `. No results →
`x_search results: 0` plus one line of the implementer's wording saying nothing matched.
Errors → `x_search error [<CODE>]: <message>`.

Details JSON: `{results: [...], queries: [...], usage: {xSearchCalls, costTicks}, code?}`;
`code` only on errors (with empty lists and zero usage).

## 6. Skill

When connected, the extension contributes a system-scope skill named `x-search` during resource
discovery (MIT `skills.md` rules: a user/project skill of the same name wins silently;
`disabled_skills` hides it). The skill text is new usage guidance written by the implementer;
it must not reuse the SUL skill's section layout, rules, examples, account names or cost figures.
If the skill file cannot be found, the tool still registers
and a warning is logged once. Not connected → no skill.

## 7. Platform notes

Plain HTTPS client; no platform-specific code. Offline mode needs no special case (the call
fails as `UPSTREAM`), but the implementer may skip registration when offline (`PI_OFFLINE`/
`MAHO_OFFLINE`), matching `mcp.md`.

## 8. Acceptance tests (written from this spec; HTTP via an injected fake)

1. Load gate: `auth.json` with `xai.type` `api_key` / `oauth` → registered; other type, invalid
   JSON, no `xai` entry → not registered even with `XAI_API_KEY` set; no `auth.json` + key set →
   registered; neither → not registered and no skill.
2. Schema: defaults applied (`latest`, 10, false, false); `additionalProperties` rejected.
3. Validation order: each row of §3 yields its code; a blank query with both handle lists set
   yields `INVALID_PARAMS`; both lists plus 21 handles yields `INVALID_FILTERS`.
4. Dates: `2024-02-29` ok, `2023-02-29`, `2024-13-01`, `0099-01-01`, `2024-1-01` →
   `INVALID_DATE`; `from_date` after `to_date` → `INVALID_DATE_RANGE`; equal dates ok.
5. Query length counted in UTF-16 units: 2000 units ok, 2001 rejected.
6. Bearer: stored credential preferred over env; stored-but-empty → `AUTH` without env fallback;
   env only → used trimmed; none → `AUTH`, no HTTP call; a credential changed between two calls
   is used by the second call.
7. Request body: exact fixed fields of §4.4; dates and handles copied into the tool object;
   understanding flags present when false; prompt contains query, `since:`/`until:` and
   `max_results`; model override env respected.
8. Status mapping: 401, 403, 429 (with and without `retry-after`), 500, 503, non-JSON 200,
   transport error, timeout (short injected deadline, no sleeps), pre-cancelled and mid-flight
   cancellation each give the §4.6 code.
9. Normalization: dedupe across annotations, text and citations keeps first-seen order; title
   fallback rules; snippet line selection; `max_results` truncation; queries extracted only from
   the listed names/types; usage fields read; garbage payload → empty result, no panic.
10. Output text: empty-result text, numbered blocks and the `Queries used:` join; error text
    format; details carry `code` only on errors.
11. Skill: contributed when connected; a user skill named `x-search` shadows it; listed in
    `disabled_skills` → not contributed; missing file → tool registered, one warning.
