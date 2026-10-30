# Sensitive-output redaction and comment-checker notes (delta record)

Covers `maho-core` (`sensitive_output`) and records the comment-checker decision. Written from
behaviour stated in the census plus the public conventions of the credential formats; no SUL
code is reproduced.

Status (review 2026-10-30, docs/cleanroom/reviews/sensitive-output-5152.md): D1 and D2 are
already satisfied on the MIT side. `crates/maho-core/src/sensitive_output.rs` is an MIT port of
senpi `core/sensitive-output.ts` and senpi-ai `utils/secret-{redaction,classifier,lexer}.ts` at
v2026.10.27-2 (branch `sync5152/omo-memory`, commit b0c3883a), which is the upstream these rules
come from. D1-D2 are kept as an acceptance description only; where they and the MIT port differ
(the port also reads percent escapes and HTML entities as the characters they encode), the MIT
port wins. No clean-room implementation is to be written from this file.

## Delta (omo v5.1.52, 2026-10-30)

### D1. Credential-shape redaction (`redact_credential_shapes(text, replacement)`)

Behaviour: one left-to-right pass, linear in the input, no backtracking. The text is split into
alternating tokens and delimiters. In a token, from the position where a recognised credential
shape **starts a word** (start of the token, or after a non-word character; word characters are
ASCII letters, digits and `_`) to the end of that credential's own alphabet run, the text is
replaced with `replacement`; everything else is byte-identical. Overlapping hits merge into one
replacement. Non-ASCII text is preserved and never splits a character.

Recognised shapes (interface facts: the public formats of these providers):

| prefix | minimum run after the prefix | run alphabet |
| --- | --- | --- |
| `sk-` | 8 | letters, digits, `-`, `_` |
| `sk_live_`, `sk_test_`, `rk_live_`, `rk_test_` | 16 | letters, digits |
| `gsk_`, `xai-`, `hf_` | 20 | letters, digits |
| `xoxa-`, `xoxb-`, `xoxo-`, `xoxp-`, `xoxr-`, `xoxs-` | 10 | letters, digits, `-` |
| `ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_` | 20 | letters, digits |
| `github_pat_` | 20 | letters, digits, `_` |
| `AKIA`, `ASIA` | exactly 16 | upper-case letters and digits |
| `AIza` | 30 | letters, digits, `-`, `_` |

Also recognised: a JSON web token, i.e. a run starting `eyJ` made of three base64url segments
joined by `.` (the first segment, `eyJ` included, at least 8 characters, the other two at least
5 each), where the third segment is not directly followed by more base64url text. A trailing run
of `.` at the end of a token is not part of the credential.

Where the match ends: at the first character outside the prefix's alphabet (so a key inside a URL
or file name is cut at its boundary); for a JWT, at the end of the token.

Errors: none; empty input returns empty output.

Tests:
1. `ghp_` + 20 letters inside a sentence → replaced; `ghp_` + 19 letters → unchanged.
2. `https://x/y?k=sk-abcdefgh12&z=1` → only `sk-abcdefgh12` replaced (the `&z=1` survives).
3. `AKIA` + 16 upper-case/digit characters → replaced; + 17 → unchanged.
4. A three-segment JWT → replaced; a two-segment `eyJ...` string → unchanged.
5. `mysk-abcdefghij` (prefix not at a word start) → unchanged.
6. A 1 MB input of repeated `sk-` completes in time linear in its size (no pathological regex).
7. Non-ASCII text around a key is preserved exactly.

### D2. Environment-assignment redaction

An assignment `NAME=value` is found by one linear match: `NAME` starts at a word boundary with
`A-Z` followed by `A-Z0-9_`, then `=`, then a value of one or more characters other than
whitespace, `'` and `"`. The name is tested **once, on that name only** (never inside the value):
if it contains any of `API_KEY`, `SECRET`, `TOKEN`, `PASSWORD`, `AUTH` (searched after its first
character), the value becomes `[REDACTED]` and the name stays; otherwise the assignment is left
as it was. This replaces the earlier rule that required the credential word inside a larger
backtracking name pattern; results for ordinary inputs are the same, but a long upper-case name
without `=` must not cause super-linear work.

`redact_sensitive_token_values(text, replacement)` is now exactly D1. `redact_sensitive_output`
applies, in order: D2, the `Authorization: Bearer <token>` header rule (case-insensitive header),
the `Bearer sk-...` rule, then D1 with `[REDACTED]`; unchanged apart from D1 now covering more
shapes.

Tests: `OPENAI_API_KEY=sk-live-abc` → `OPENAI_API_KEY=[REDACTED]`; `PATH=/usr/bin` unchanged;
`HOME_DIR=/x` unchanged; `A=1 MY_TOKEN=2` → only the second redacted; `TOKEN_X=2` unchanged (the
word test skips the first character); a 200 000-character upper-case word with no `=` completes
quickly.

### D3. Comment checker cache-dir name

N-A. The MIT checker (`maho-comment-checker`) finds an external executable (override variable,
packaged location, `PATH`) and keeps no download cache; a cache-root name that other tools could
share does not exist in its design.

### D4. Mapping table

| census row | delta § | APPLIES/N-A | reason |
| --- | --- | --- | --- |
| omo-memory-30 | D1 | APPLIES (satisfied by MIT port b0c3883a) | redaction output is user/log visible and a security boundary |
| omo-memory-31 | D2 | APPLIES (satisfied by MIT port b0c3883a) | same module, name test and delegation |
| omo-components-08 | D3 | N-A | N-A reason: no cache directory in the MIT comment-checker design |

Leak self-check:
- No fenced SUL/TS/Rust code (none used).
- No sentence paraphrases a SUL comment; wording is the writer's.
- The prefix table is an interface fact (public key formats), marked as such.
- No OmO-original identifiers; function names are the existing MIT names.
- No OmO-original features.
