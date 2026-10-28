# Spec: model selection, model profiles and fallback (Phase 1)

Status: draft for review · Audience: clean-room implementer · Proposed crates:
`maho-model-select` (pure functions: reference grammar, matching, delegated-task choice, error
classes) and `maho-model-profile` (session-start extension). Names are suggestions; keep them
neutral (README rule 4).

Plan reference: "모델 폴백 — 일반 → senpi 포팅" and Phase 1 step 4. Part of this area is **already
MIT in mhc** and must be reused, not rewritten:

- `maho-core::retry_fallback` — mid-turn retry/fallback for the main session (settings
  `retry.fallbackChains` in `~/.omo/agent/settings.json`, cooldowns, usage-limit/billing
  detection, probe-back), from senpi's `retry-fallback` (MIT).
- `maho-ext-model-fallback` — the `/fallback` command and `--no-model-fallback` flag.
- `maho-ext-recommended-models` — MIT list of recommended model ids with per-model provider
  rankings and default thinking levels.

This spec covers what is **not** yet MIT: model reference grammar used by `omo.jsonc`, catalog
alias use, availability matching, the delegated-task model choice, the session-start model
profile, and the error classes used by later-phase consumers.

---

## 1. Inputs every function works on

- **Available models**: an ordered set of `provider/model-id` strings — what the live model
  registry reports as usable (has credentials). Order is the registry's order and matters for
  tie-breaking.
- **Connected providers** (cold start only): provider ids known to be connected when the model
  list is not yet available.
- **Configuration view**: the output of `load_native` from [config.md](config.md) (catalog aliases
  already resolved, legacy fields already folded).

---

## 2. Model reference grammar

A *model reference* in configuration is either a string or an object.

String forms:
- `provider/model` — provider-qualified. The provider is everything before the **first** `/`;
  the model id may itself contain `/` (gateway ids such as `vercel/openai/gpt-x`).
- `model` — bare id; may be served by any provider.
- Either form may carry a reasoning suffix `:<level>` where level is one of `off`, `minimal`,
  `low`, `medium`, `high`, `xhigh`, `max`, `auto` (case-insensitive, surrounding spaces
  ignored). The suffix is recognised only when the part before the last `:` is non-empty.
  `:max` is recognised only when the reference is provider-qualified, unless the caller
  explicitly allows it (parsers of `provider/model` strings allow it).
- Legacy forms `model (level)` and `model level` (a trailing word made of a letter followed by
  letters, digits, `_` or `-`) are accepted when reading; config loading canonicalises them to
  `:level` when the token is a known level (config.md §4.5). When parsing a delegated-task
  model, a parenthesised token is taken as the variant even if it is not a known level; a spaced
  trailing token is taken as the variant (lower-cased) only after the `:level` check fails.

Object form: `{ model, reasoning?, temperature? (0–2), top_p? (0–1), max_tokens? (>0),
provider_options? }` (plus legacy fields folded on read).

Reasoning normalisation: trim, lower-case; empty → none; `none` → `off`; a known level or `auto`
→ that level; anything else → passed through as an opaque string.

Clamping (for providers that support only some levels): given a requested level and the allowed
list, return the highest allowed level that is not above the requested one in the order
`off < minimal < low < medium < high < xhigh < max`; `None` when nothing qualifies or the request
is not a level.

Normalisation helpers:
- `normalize_model(opt)`: trimmed string or `None` when empty/absent.
- `normalize_model_id(id)`: replace every `.` that is followed by a digit with `-`
  (`gpt-5.4` → `gpt-5-4`).
- `parse_model_string(s) -> {provider, model_id, variant?}`: requires a non-empty provider and
  model part around the first `/`; variant extracted as above (allowing `:max`).

---

## 3. Availability matching (fuzzy)

`match_available(target, available, providers?) -> Option<String>` returns one entry of
`available` or none.

1. Normalise names for comparison: lower-case; then treat version separators as equivalent for
   these families by rewriting `-` between two version numbers to `.`: `claude-(opus|sonnet|
   haiku)-N-M` → `…-N.M`; `kimi-k2-N` → `kimi-k2.N`; `glm-N-M` and `gpt-N-M` (at a word
   boundary) → `…-N.M`. (These are compatibility facts about how providers publish ids.)
2. If `providers` is a non-empty list, consider only entries whose provider (text before the
   first `/`) is in the list; otherwise consider all.
3. Keep candidates whose normalised full string **contains** the normalised target.
4. If a candidate's normalised full string **equals** the normalised target → return the first
   such candidate.
5. Else, among candidates whose normalised *model id* (text after the first `/`; empty for an
   unqualified entry) equals the target → return the best by (provider rank, shorter model id).
6. Else return the best of all candidates by the same ordering.

Provider rank = index of the candidate's provider in `providers` (when given; providers not in
the list rank last); without `providers` every rank is equal. Ties keep the earlier entry in
`available` order.

`is_available(target, available)` = `match_available(target, available, None).is_some()`.

---

## 4. Provider-specific model id transforms

`model_id_for_provider(provider, model_id) -> String` adapts a canonical model id to how a
provider publishes it. Compatibility facts:

- **Vercel AI gateway** (`vercel`): ids are `<sub-provider>/<model>`. If the id already contains
  `/`, keep the sub-provider and transform the model part; otherwise infer the sub-provider from
  the model prefix: `claude-` → `anthropic`, `gpt-` → `openai`, `gemini-` → `google`, `grok-` →
  `xai`, `minimax-` → `minimax`, `kimi-` or `k3` → `moonshotai`, `glm-` → `zai`; unknown prefix →
  unchanged. The model part gets the Claude dot rule and the Gemini 3.1 Pro rule below.
- **GitHub Copilot** (`github-copilot`): Claude dot rule, then Gemini 3.1 Pro rule, then Gemini 3
  Flash rule.
- **Google** (`google`): Gemini 3.1 Pro rule, then Gemini 3 Flash rule.
- **Kimi coding** (`kimi-coding`, `kimi-for-coding`): `kimi-k3` → `k3`, `kimi-k3-256k` → `k3-256k`.
- Every other provider: unchanged.

Rules:
- *Claude dot rule*: `claude-<name>-<N>-<M>` → `claude-<name>-<N>.<M>`.
- *Gemini 3.1 Pro rule*: an occurrence of `gemini-3.1-pro` not immediately followed by `-` becomes
  `gemini-3.1-pro-preview`.
- *Gemini 3 Flash rule*: an occurrence of `gemini-3-flash` not immediately followed by `-` and not
  immediately preceded by `antigravity-` becomes `gemini-3-flash-preview`.

When a reference names `provider/model` and the model already starts with `provider/`, strip that
prefix once before transforming.

---

## 5. Delegated-task model choice

Used when a subagent/category task needs a model (Phase 2 task) and by the doctor check.

### 5.1 Inputs
- `user_model` (optional): the first entry of the category/agent `models` list, else its
  `model` (see §5.4).
- `user_fallbacks` (optional list of strings): the remaining `models` entries, else
  `fallback_models` flattened to strings.
- `category_default` (optional): a built-in default model for the category (Phase 2 data).
- `category_default_is_user_configured` (bool).
- `chain` (optional list of rungs `{ providers: [ids], model, variant? }`): a built-in fallback
  chain (Phase 2 data).
- `allow_unlisted_providers` (bool).
- `available` (ordered set), `system_default` (optional).
- Cache state: `connected_providers` (optional), `has_model_cache`, `has_connected_cache`;
  optional logger.

### 5.2 Output
`Resolved { model: "provider/id" or bare, variant?, rung?, from_fallback: bool }`, or
`Deferred` (no cache information at all — caller retries later), or nothing (no model).

### 5.3 Algorithm (first match wins)
1. **User model present** → parse it (§2). Let primary = parsed `provider/id` and variant if a
   variant was present, else the raw string. If `available` is non-empty and `user_fallbacks` is
   non-empty and the primary does **not** match (§3, provider hint = its provider), try each
   fallback in order (parsed; provider hint = its provider) and return the first that matches as
   `from_fallback=true` (log that the user's primary was unreachable). Otherwise return the
   primary (not validated against availability) with its variant.
2. **Cold cache** (`available` empty): if there are no connected providers and neither cache
   flag is set → `Deferred`.
3. **Category default** present:
   a. user-configured → return it as written (variant split off if present), no validation;
   b. cold cache → if connected providers are known and the default's provider is not among
      them, skip (log) and continue; otherwise return the default as written;
   c. warm cache → match it (provider hint = its provider). Return the match — except when the
      default's last path segment ends in `-high` (and is longer than `-high`) and the match
      differs from it, in which case return the default as written.
4. **User fallbacks**: for each (parsed): cold cache → skip it when connected providers are known
   and none of its provider hint is connected, otherwise return it unvalidated; warm cache →
   return the first match. All with `from_fallback=true`.
5. **Chain**:
   - cold cache without connected providers → the first rung's first provider + transformed
     model (§4) with the rung's variant;
   - cold cache with connected providers → the first rung (in order) having a connected
     provider: `provider/transformed-model`, rung variant (log);
   - warm cache → for each rung, for each listed provider: match `provider/transformed-id`
     restricted to that provider; if none and the transformed id differs from the rung id, try
     `provider/rung-id`. On a match: if the category default was an explicit `-high` model and
     the rung's variant is `high` and the match equals the default without `-high`, return the
     explicit `-high` name without variant; else return the match with the rung variant. If no
     listed provider matched and `allow_unlisted_providers` is set, match the bare rung model
     against available entries whose provider is not listed by any **later** rung of the same
     model.
   All chain results carry the rung and `from_fallback=true`.
6. **System default** (normalised) → return it.
7. Nothing.

### 5.4 Mapping from configuration (consumer contract for Phase 2)
For category `C` with user definition `U` (from the view) and built-in data `B`:
- if `U.models` exists: `user_model = first entry's model`, `user_fallbacks = remaining
  entries' models`; the reasoning of the selected entry becomes the task's reasoning;
- else `user_model = U.model`, `user_fallbacks = U.fallback_models` (string → one element; list
  of strings/objects → their `model`s);
- `category_default = B.default`, `chain = B.chain`, `allow_unlisted_providers = false`,
  `category_default_is_user_configured = false`;
- variant precedence for the chosen model: `U.variant` (legacy) → resolved variant →
  built-in variant; reasoning: selected entry's reasoning → `U.reasoning` → legacy
  `U.reasoningEffort`;
- the runtime retry chain for the child = user candidates, then remaining built-in rungs after
  the selected one, keeping only entries that are available.
- A category whose built-in chain has no resolvable rung and for which the user configured no
  `model`/`models`/`fallback_models` and no system default is supplied is reported as
  unavailable (with the attempted chain and the missing providers), not silently defaulted.

Built-in category names, default models and chains are Phase 2 data; they are not specified
here (see Q4).

### 5.5 Doctor check (MIT replacement of `cli/doctor.rs` `category_lines`)
For each built-in category chain, report whether some rung resolves (§5.3 step 5, warm cache,
using the registry's available list from `<agent dir>/models.json` + `auth.json`, after
registering the subscription providers). One PASS/WARN/FAIL line per category; exact wording is
the implementer's.

---

## 6. Model profiles (main-session model at session start)

A *model profile* is a named, ordered chain of models for the **main** session, chosen by a
human-friendly id instead of a model id. It is unrelated to overlay `profiles` (config.md §6.2).

### 6.1 Configuration (interface facts)
- `model_profile` (string): either a profile id, or — when it contains `/` — a literal
  `provider/model[:level]` **pin**.
- `model_profiles.<id>`: `{ display_name?, family? ("daily"|"geeky"), tier? ("normal"|"heavy"),
  models?: [string | {model, reasoning?…}] }`.
- Built-in ids that must exist (users' files name them): `daily-normal`, `daily-heavy`,
  `geeky-normal`, `geeky-heavy`, in that order, with family/tier matching the id. The retired
  id `recommended` is treated as "unset" unless the user defines a profile with that name.
- Built-in chains: each rung is `{ providers (ranked list), model, reasoning }`. **Source the
  chain contents from MIT data** (`maho-ext-recommended-models`: its model ids, provider
  rankings and default thinking levels), not from the SUL table. Suggested composition (to be
  confirmed by the maintainer): `daily-normal` = the recommended general model at medium with
  the recommended alternates as later rungs; `daily-heavy` = the recommended top model at high
  effort; `geeky-*` = the recommended GPT-family models. Display labels are the implementer's
  own (derive from family/tier).

### 6.2 Merging user definitions
Start from the built-ins in their order; then for each `model_profiles` entry: it **replaces**
the built-in of the same id wholesale (no per-field merge of `models`), or is appended as a new
id. `display_name`, `family`, `tier` fall back to the replaced built-in's values when omitted;
`display_name` falls back to the id for new profiles. A user entry without `models` therefore
has an **empty** chain.

User rung parsing: a string `provider/model[:level]` → providers `[provider]`, model, reasoning;
a bare `model[:level]` → **no providers** (served by any provider); object form uses `model` and
`reasoning` (object reasoning wins over a suffix). Empty reasoning is dropped.

### 6.3 Resolution (pure)
`resolve_profile(profiles_cfg, active, available) ->`
- `Resolved { profile, provider, model_id, reasoning?, skipped: [rung labels] }`
- `Unavailable { profile, chain: [rung labels] }`
- `Empty { profile }`
- `Unknown { name, known: sorted ids, message }`

Steps:
1. `active` trimmed. If it contains `/` → pin: parse as a user rung; if `available` is
   non-empty and it matches (§6.4 builtin-style matching) → `Resolved` (source pin, no skipped);
   else `Unavailable` with the pin as the only chain entry.
2. Look up the merged profile; missing → `Unknown` (message names the id and lists known ids).
3. No rungs → `Empty`.
4. If `available` is non-empty, walk rungs in order; the first rung that matches → `Resolved`
   with the labels of the rungs skipped before it. Otherwise `Unavailable` listing every rung.
   Rung label = `firstprovider/model`, or the bare model.

### 6.4 Rung matching
- Rung with **no providers** → `match_available(model, available, None)`; split the result at
  the first `/`.
- **User-defined** profile rung **with** providers → exact only: the first listed provider for
  which `provider/model` is literally in `available`.
- **Built-in** rung (and pins) with providers → for each provider in rank order: candidate ids =
  transformed id (§4) and, if different, the rung id as written; return the first
  `match_available("provider/id", available, [provider])`. A built-in rung never matches through
  a provider it does not list.

### 6.5 Default when `model_profile` is unset (or retired)
Try `daily-normal`, `daily-heavy`, `geeky-normal`, `geeky-heavy` in order (each with the
§6.6 probe, sharing exclusions); use the first `Resolved`; if none resolves, report the result of
`daily-normal`. The default is never written back to configuration.

### 6.6 Request-auth probe (skip what the first turn could not use)
The registry lists providers with *stored* credentials, including logins that can no longer
refresh. Before applying a resolved non-pin candidate:
1. Determine credential slots for the provider: if rotation is not allowed → only the default
   credential; else if one account is pinned → only it; else every account (or the default when
   there are none). Rotation is not allowed when the provider's auth source is a runtime API key,
   or when `<agent dir>/models.json` (JSONC) sets `providers.<id>.credentials.rotation` to
   `false`; an unreadable file means rotation allowed.
2. Resolve provider auth for each slot until one succeeds (a provider that needs no credential
   counts as success). If none succeeds → failure with reason **refresh** when the error's
   machine code is `oauth`, else **credentials**; exclude the whole provider.
3. Resolve the model's request auth (API key + headers) on the slot that succeeded. Failure →
   reason **request**; exclude only that `provider/model`.
4. Re-resolve with the exclusions and repeat until a candidate passes or nothing resolves. Each
   failure is logged as a warning with provider, model, reason and an error-kind fingerprint
   built from the error's class and machine code only (never the message, which may contain
   secrets). With a debug env flag set, also log a sanitised, length-capped detail.
5. A pin is never probed (explicit user choice).

### 6.7 When to apply
On `session_start`, apply only when all hold:
- the start reason is a fresh start or a new session (not resume, fork or reload);
- the session reports an initial-model provenance that is neither `cli` (a `--model` flag) nor
  `scoped`; a missing provenance counts as explicit (do not apply);
- the run is not the interactive TUI (rpc/desktop, app-server, json and print modes apply) — see
  Q2;
- the profile was not already applied for this session id (one apply per session id; one per
  extension instance when no id is available).

Then load the configuration view for the session cwd and resolve (`model_profile` set and not
retired → that id/pin; else the default order).

### 6.8 Effects
- `Resolved`: look the model up in the registry; if gone → warning notice ("resolved X but the
  registry no longer lists it") and stop. Otherwise set the session model with the
  **session-only** setter (never the persisting one: `settings.json` must not be written, or the
  profile would become a pin), and set the session thinking level from the reasoning
  (`none`→off; `auto` or unknown → leave unchanged). Send an "applied" notice and log it.
- `Unavailable` / `Empty` / `Unknown`: send a notice and log a warning; keep the current model.
- Notices are custom session messages displayed to the user, with a type id and details:
  - applied: profile id, selected `provider/model`, reasoning, skipped rung labels, and the auth
    failures (`provider`, `model`, `reason`) if any;
  - unavailable: profile id and auth failures when the reason was auth;
  - unknown: the unknown-profile message.
  Content facts: profile id (and display label when different), the selected model and
  reasoning, skipped rungs, per-failure advice (refresh → re-authenticate that provider: in
  rpc/desktop via provider settings, otherwise via an interactive login; credentials → check that
  provider's credential configuration; request → check that model's headers in `models.json`),
  and a note that failures later in the session are handled by the retry chains configured in
  settings. Wording and the
  product prefix (`mhc`) are the implementer's.
  Proposed neutral type ids: `model-profile:applied`, `model-profile:unavailable`,
  `model-profile:unknown` (no consumer in mhc reads the old ids; Q5).
- Retired id configured: log once per id per process that it is retired and the default lane is
  used.
- No session-model setter available on this runtime → log a warning and do nothing.

---

## 7. Runtime fallback and retry rules

### 7.1 Main session (already MIT)
Mid-turn retries and model fallback for the main session are provided by
`maho-core::retry_fallback` and configured by `retry.fallbackChains` in the agent settings. The
model profile chain is **start-time only**; it is not wired into the retry chains (Q3). Do not
re-implement retry logic.

### 7.2 Error classes for later-phase consumers
Phase 2 consumers (memory worker model calls, delegated children) need two predicates. Specify
them as pure functions over a provider error `{ name?, message?, status? }` (nested `data`,
`error`, `cause`, `detail` records may carry the same fields; the first non-empty name and the
first numeric status win):

- `is_context_overflow(message)`: true for "context/prompt too long / maximum context length
  exceeded / too many tokens"-type messages, false for messages that merely mention limits of a
  different kind (rate limits, output token caps).
- `classify(error) -> abort | context_overflow | missing_api_key | invalid_api_key |
  model_not_found | quota_exceeded | none`:
  - *abort*: an explicit abort error name, **or** a *terminal* quota/billing exhaustion (an error
    type `terminal_quota_exhausted`, or wording such as "terminal quota", "terminal billing
    limit", "hard billing limit" that is not negated as "non-terminal");
  - *context_overflow*: an error named as context overflow;
  - *missing_api_key*: an API-key-loading error name, or a message saying an API key is missing
    from an environment variable;
  - *invalid_api_key*: a message saying the API key must be a string;
  - *model_not_found*: a model-not-found error name, or an unknown error whose message says
    "model not found";
  - *quota_exceeded*: quota/billing/credit/usage-limit error names, or messages about exceeded
    quota, insufficient balance/credits, plan/session/daily/weekly/monthly limits, payment
    required, resource or capacity exhausted (including common Chinese-language provider
    messages for usage caps and insufficient balance).
- `is_retryable(error, retry_status_codes = [429, 500, 502, 503, 504]) -> bool`, first rule wins:
  1. abort or context_overflow → false;
  2. an error named as a content-filter rejection → true;
  3. missing_api_key, model_not_found, quota_exceeded → true (switch model);
  4. status in `retry_status_codes` (status may also be read from a bare code in the message) →
     true;
  5. an explicit provider "retryable" flag → true if there is no status or the status is safe
     (5xx, 408, 425, 429, or listed); otherwise false and an optional callback is notified;
  6. message mentions rate limiting, too many requests, quota reset, usage exceeded, capacity
     exhausted, credentials cooling down, model not supported, service unavailable, overloaded,
     temporarily unavailable, "try again", a standalone 429/503/529, or the Chinese-language
     equivalents → true;
  7. otherwise false.
- `is_provider_exhaustion(error)` = `classify(error) == quota_exceeded` (eligible to move to the
  next provider).

Before writing these, check whether `maho-core::retry_fallback::{usage_limit, billing}` already
classify the same cases; reuse and extend them instead of adding a parallel classifier. Only the
memory worker currently needs these predicates; if the memory port (Phase 2) does not, skip
§7.2 entirely (Q7).

### 7.3 Delegated-task call errors (Phase 2, contract only)
When the `task` tool call itself fails validation (missing required argument, both or neither of
category/agent given, unknown category/agent/skill, calling a primary agent, empty agent name),
the tool result is classified by error type and a corrective hint is appended, including the
"Available …: <list>" line from the error output when present. The hint text and example call
must be written new and must not mention persona agent names.

---

## 8. Required API surface and consumers

| Capability (suggested name) | Consumer (today → future MIT) |
| --- | --- |
| `normalize_model`, `normalize_model_id`, `parse_model_string`, `parse_variant`, `split_reasoning_suffix`, `normalize_reasoning`, `clamp_reasoning` | config migrator (config.md §12.2), delegated choice, model profile, Claude-compatible agent/command loaders (Phase 3) |
| `match_available(target, available, providers)`, `is_available` | delegated choice, model profile, doctor |
| `model_id_for_provider(provider, id)` | delegated choice, model profile, task adapter (Phase 2) |
| `resolve_delegate_model(input, cache_state) -> Resolved, Deferred or None` | `cli/doctor.rs` category check (replace `delegate_core::resolve_model_for_delegate_task`), Phase 2 category resolver |
| `ModelProfileExtension` (session_start) + pure `resolve_profile`, `merge_profiles`, `default_profile_order` | mounted by the MIT omo-glue replacement (today `maho-omo` component list registers `model-profile`); replaces the removed `maho-ext-quiet-model-profile` (rewrite it as a thin re-export or drop it) |
| credential-rotation policy reader (`<agent dir>/models.json`) | model profile |
| `classify_error`, `is_retryable`, `is_context_overflow`, `is_provider_exhaustion` | Phase 2 memory worker / task children (only if needed, §7.2) |
| optional advisory predicate for selectors a provider does not serve | `config-notices` (config.md §13) |

Host APIs the profile extension needs from `maho-ext-api` (already MIT): `session_start` event
with reason and initial-model provenance, extension mode, `ModelRegistry` (`get_available`,
`find`, credential accounts, provider auth status, slot-aware provider auth and model request
auth), session actions (`set_session_model` session-only, `set_session_model_thinking_level`),
message actions (`send_message` with custom type, display flag and details), component logger.

---

## 9. Test cases (given / when / then)

Grammar
1. `parse_model_string("openai/gpt-x:max")` → provider `openai`, id `gpt-x`, variant `max`.
2. `split_reasoning_suffix("gpt-x:max")` (no provider, default) → no level; `"a/gpt-x:MAX"` →
   level `max`.
3. `parse_variant("claude-opus (thinking)")` → id `claude-opus`, variant `thinking`.
4. `normalize_reasoning(" None ")` → `off`; `"turbo"` → passthrough `turbo`.
5. `clamp_reasoning("xhigh", ["low","high"])` → `high`; `("minimal", ["high"])` → none.

Matching
6. available `["acme/kimi-k2-7"]`, target `kimi-k2.7`, providers `["acme"]` → match.
7. available `["acme/gpt-7.2"]`, target `gpt-7-2` → match.
8. available `["sub/claude-sonnet-9", "gw/claude-sonnet-9"]`, target `claude-sonnet-9`, no
   providers → `sub/…` (first); providers `["gw","sub"]` → `gw/claude-sonnet-9`.
9. available `["gw/claude-haiku-9-2030", "sub/claude-haiku-9"]`, target `claude-haiku` →
   `sub/claude-haiku-9` (shorter model id).
10. available `["acme/widget-mini", "widget-with-a-long-suffix"]`, target `widget` →
    `widget-with-a-long-suffix` (an unqualified entry has an empty model id and wins the length
    tie-break) — same result with the order reversed.

Transforms
11. `("vercel","gpt-7")` → `openai/gpt-7`; `("vercel","grok-9")` → `xai/grok-9`;
    `("vercel","custom/abc")` → `custom/abc`; `("vercel","mystery-1")` → `mystery-1`.
12. `("github-copilot","claude-sonnet-8-1")` → `claude-sonnet-8.1`.
13. `("google","gemini-3-flash")` → `gemini-3-flash-preview`; `("google",
    "antigravity-gemini-3-flash")` → unchanged.
14. `("kimi-for-coding","kimi-k3-256k")` → `k3-256k`; `("acme","kimi-k3")` → unchanged.

Delegated choice
15. user model `a/x`, fallbacks `["b/y"]`, available `{b/y}` → `b/y`, from_fallback.
16. user model `a/x:high`, available empty → `a/x` with variant `high` (no validation).
17. no user model, available empty, no connected providers, no cache flags → Deferred.
18. category default `acme/gpt-x-high`, available `{acme/gpt-x-high-preview}` →
    `acme/gpt-x-high` (explicit `-high` kept); category default `acme/gpt-x`, available
    `{acme/gpt-x-preview}` → `acme/gpt-x-preview`.
19. cold cache, connected `["kimi-for-coding"]`, chain `[{["acme"],"m1"},
    {["kimi-for-coding"],"kimi-for-coding/kimi-k3-256k","low"}]` → `kimi-for-coding/k3-256k`,
    variant `low`, rung = the second rung.
20. warm cache, chain rung `{["vercel"], "claude-sonnet-8-1"}`, available
    `{vercel/anthropic/claude-sonnet-8.1}` → that entry.
21. nothing matches, system default `" p/m "` → `p/m`.

Model profiles
22. unset profile, available `{anthropic-subscription/<daily-normal top model>}` → daily-normal
    resolves; with only a GPT-family model available → the first geeky lane that resolves; with
    nothing → result reports daily-normal Unavailable.
23. `model_profile: "p/m:high"`, available `{p/m}` → pinned, reasoning `high`, not probed.
24. user `model_profiles.daily-normal = {display_name: "Mine"}` → Empty (no inherited chain).
25. user profile rung `"p/m"` with available `{p/m-2024}` → no match (exact for user scoped
    rungs); bare rung `"m"` → matches `p/m-2024`.
26. `model_profile: "nope"` → Unknown listing sorted known ids.
27. `model_profile: "recommended"` (not user-defined) → treated as unset; logged once.
28. probe: provider A's only login fails with code `oauth` → A excluded, next rung chosen,
    applied notice lists A with reason refresh.
29. probe: provider auth ok, model headers fail → only that model excluded.
30. rotation disabled for A in `models.json` → only the default credential is probed.
31. session start with reason resume → nothing applied; with provenance `cli` → nothing; in TUI
    mode → nothing; twice for the same session id → applied once.
32. after apply, `settings.json` is unchanged and the session thinking level equals the rung's
    reasoning.

Errors (only if §7.2 is implemented)
33. `{error: {name: "content-filter-error"}}` → retryable; `{name: "BadRequest", status: 400,
    message: "unknown content filter option"}` → not retryable.
34. message `"failed: capacity_limit reached"` → quota_exceeded, retryable; message
    `"capacity_limits table"` → not quota.
35. `{detail:{error:{type:"terminal_quota_exhausted"}}}` → abort, not retryable.
36. status 418 with retryable flag true → not retryable, callback notified.

---

## 10. Open questions

- **Q1** Built-in profile chain contents: confirm sourcing them from
  `maho-ext-recommended-models` (MIT) and the per-lane composition in §6.1. The `daily`/`geeky`
  and `normal`/`heavy` values must stay accepted because users' files contain them.
- **Q2** Skipping the profile in the interactive TUI is a product decision of the old code (no TUI
  surface for lanes). Keep it, or apply in TUI too now that the profile is ours?
- **Q3** Wire the profile chain into `retry.fallbackChains` for mid-session fallback, or keep it
  start-time only (current behaviour)?
- **Q4** Built-in category names used by the delegated choice and the doctor check
  (`quick`, `deep-low`, `deep-high`, `writing`, `visual-engineering`, `unspecified-low/high`,
  `artistry`, `architect`, `ultrabrain`, …) are Phase 2 data. Some names may be OmO coinages;
  users' configs reference them as keys, so the MIT task port must decide which built-ins to ship
  under which names (user-defined categories keep working regardless).
- **Q5** Rename the custom message type ids to neutral `model-profile:*`? No mhc consumer reads
  the old ids; old session transcripts would show the old ids as unknown custom messages.
- **Q6** The provider-specific startup advisory (a provider that serves only some lanes of a model
  family) is narrow product knowledge; keep it as an optional predicate or drop it.
- **Q7** Implement §7.2 in Phase 1, or defer it to the memory/task ports that need it?
- **Q8** The doctor check registers two subscription providers before listing models; confirm
  the MIT provider ids for that step (`anthropic-subscription` / `chatgpt-subscription` naming vs
  the legacy ids renamed in config.md §12.2).

---

## Leak self-check (spec writer)

- Algorithms are described step by step in prose; no code, regex sources or comments are copied.
  The quota/retry message categories are summarised by meaning, not reproduced as pattern lists.
- Provider transforms and family normalisation rules are stated as compatibility facts about
  provider id formats.
- The built-in profile chain table, its descriptions and display labels from the SUL source are
  **not** reproduced; only the profile ids and enum values users write are kept, and chain data
  is sourced from MIT code.
- Notice/log wording is specified by required facts; proposed identifiers are neutral.
- No OmO-original feature or persona name is used.
