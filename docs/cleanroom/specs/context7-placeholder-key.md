# Context7 placeholder API keys — behaviour spec (unit `context7-placeholder-key`)

Scope: how the MIT-profile builtin MCP crate `crates/maho-mcp-builtins` decides whether the
`CONTEXT7_API_KEY` environment variable holds a usable API key. A variable that carries a
placeholder value must leave the `context7` server anonymous, exactly as if the variable were
unset; a variable that carries a real key must enable bearer auth from that variable.

Sources this spec is written from: the upstream OmO opencode edition predicate
`packages/omo-opencode/src/mcp/context7.ts` at pin `354bcf9f` (the normalisation helper at lines
22–23 and the placeholder test at lines 26–29), and the public Context7 client documentation,
which shows `<YOUR_API_KEY>` as the value copied from its `.env` templates. The SUL-licensed
legacy builtin-MCP component was consulted only after this rule was written down, as a
cross-check that the behaviour was understood; it is not a source for this document or for the
MIT implementation. Everything below is observable behaviour, stated without code.

## 1. Why this rule exists

Context7 answers anonymous requests. Sending a template value such as `<YOUR_API_KEY>` as a
bearer token turns that working anonymous session into an authentication failure, so the user
sees a broken server after copying a documented `.env` snippet and never filling it in. The
builtin therefore treats a small set of placeholder spellings — and only those — as "no key",
and treats everything else as a key the user meant to supply.

## 2. Recognising a placeholder

A raw value is a placeholder exactly when its **normalised form** is empty or is the phrase
`your api key` (two words, one space each, lowercase). Normalisation is defined as follows, in
order:

1. Remove leading and trailing whitespace from the raw value.
2. Discard every angle bracket, double quote, single quote and backtick character anywhere in
   the remainder. These are the characters `.env` templates and shells use to wrap a value.
3. Replace **every run** of one or more whitespace characters, underscores or hyphens with a
   single space. A run at the end of the string still becomes one space; it is not dropped.
4. Lowercase the result (Unicode-aware; ASCII keys behave as expected).

Two consequences matter for the implementation:

- The replacement in step 3 is what makes a naive equality test insufficient. `your_api_key`,
  `YOUR-API-KEY`, `Your API Key` and `<YOUR_API_KEY>` all normalise to the placeholder phrase,
  even though none of them equals it before normalisation.
- Because the final separator run is replaced (not trimmed away), a normalised form that is
  `your api key` followed by one trailing space belongs to a **real** key. So `your api key-`
  and `your_api_key_` normalise to `your api key ` and are real, while `your_api_key ` (trailing
  space) loses that space in step 1 and is a placeholder. Anyone tightening this rule later must
  preserve that distinction: a trailing separator run is significant.

An empty raw value, or one that is only whitespace, normalises to the empty string and is a
placeholder. A value consisting only of discarded bracket/quote characters also normalises to
the empty string and is a placeholder.

The variable being absent is not a placeholder: it is simply unset. Both produce the same
observable outcome.

## 3. Case table

| Raw value | Normalised form | Decision |
| --- | --- | --- |
| absent | — | no key |
| `""` (empty) | (empty) | no key |
| `"   "` (whitespace only) | (empty) | no key |
| `<YOUR_API_KEY>` | `your api key` | no key |
| `your_api_key` | `your api key` | no key |
| `YOUR-API-KEY` | `your api key` | no key |
| `  your api key  ` | `your api key` | no key |
| `"Your API Key"` | `your api key` | no key |
| `your_api_key_` | `your api key ` | real key |
| `your api key-` | `your api key ` | real key |
| `ctx7sk-abc123` | `ctx7sk-abc123` | real key |
| `  real-key  ` | `real-key` | real key |

The decision is a single predicate over the value; there is no separate "looks long enough"
or "starts with a known prefix" check, because Context7 keys have no documented prefix that
would make one safe.

## 4. Observable outcome

The predicate runs while the builtin settings are resolved, before any declaration is produced.
It decides only whether the `context7` token variable counts as present; it never rewrites the
stored value, because the declaration names the variable and the MCP client reads the
environment itself.

- Placeholder or absent, with `context7` otherwise enabled (not switched off, not offline, not
  disabled by config): the `context7` declaration is still produced, with
  `McpAuth::Disabled` and `bearer_token_env: None`. The server is anonymous.
- Real key: the `context7` declaration carries `McpAuth::Bearer` and
  `bearer_token_env: Some("CONTEXT7_API_KEY")`.
- `grep_app` has no token variable and is always declared with `McpAuth::Disabled` and
  `bearer_token_env: None`, whatever `CONTEXT7_API_KEY` holds.

All the existing on/off rules (`MAHO_BUILTIN_MCPS`, offline mode, `mcp.builtins`,
`mcp.disabled_builtins`) are unchanged and apply before the auth decision: a disabled builtin
is not declared at all.

## 5. Acceptance tests

Two tests in `crates/maho-mcp-builtins/tests/builtins.rs` pin this behaviour, both resolving
settings from a temporary home so the test runner's own environment cannot interfere:

1. `placeholder_context7_key_is_rejected` — for every placeholder form in §3 (absent is covered
   by the existing default test; the list here is the empty string, whitespace only,
   `<YOUR_API_KEY>`, `your_api_key`, `YOUR-API-KEY`, `  your api key  ` and
   `"Your API Key"`), the resolved `context7` declaration has `auth == Some(McpAuth::Disabled)`
   and `bearer_token_env == None`.
2. `real_context7_key_is_kept` — for a realistic key and for the two trailing-separator forms
   `your_api_key_` and `your api key-`, the resolved `context7` declaration has
   `auth == Some(McpAuth::Bearer)` and
   `bearer_token_env == Some("CONTEXT7_API_KEY")`. These cases guard the "trailing separator is
   significant" rule from §2.

## Leak self-check

- No code, comments or identifiers from the SUL component; the rule is described as an
  ordered prose procedure derived from the upstream opencode-edition predicate.
- The only literals named are environment-variable names, the placeholder phrase, the
  documented template value and the two test names — all interface facts a user can observe.
