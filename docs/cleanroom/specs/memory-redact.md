# Secret redaction — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/sync/redact.rs`. Implemented from scratch in
`crates/maho-memory/src/redact.rs`, with no regex engine: the scanner walks the string itself.

Memory is long-lived and shared between sessions, so a credential that reaches it leaks
permanently. Every candidate value is scanned before it is stored, and each match is replaced by
`***`.

## What counts as secret-like

| Class | Shape |
|---|---|
| `AwsAccessKey` | `AKIA` followed by exactly 16 uppercase ASCII letters or digits, at a word boundary |
| `CredentialAssignment` | `token`, `secret`, `password`, `passwd`, `pwd`, `api_key`, or `bearer`, then `=` or `:`, then a value |
| `AuthorizationHeader` | `Authorization: Bearer <token>` |
| `OpenaiKey` | `sk-` followed by a token run |
| `VendorToken` | `ghp_`, `github_pat_`, `glpat-`, `xoxb-`, `xoxp-`, `xoxa-`, `xoxr-`, `xoxs-` followed by a token run |
| `PemBlock` | `-----BEGIN … PRIVATE KEY-----` through the matching `-----END … PRIVATE KEY-----` |
| `SplitCredentialAssignment` | the same credential names spelled out with spaces between letters |

## What is deliberately not a secret

A value is skipped when it is a reference or an obvious placeholder: `process.env.…`,
`os.environ…`, `env.…`, `${…}`, `$VAR`, `{{…}}`, `[redacted]`, `[filtered]`, and the words
`bearer`, `changeme`, `placeholder`, `example`, `none`, `null`, `undefined`, `true`, `false`.

The bare words `secret` and `token` are not matches on their own — only an assignment is. A note
that says "this is a secret project" is not a credential, and redacting it would destroy real
memory.

## Values

A value runs to the next whitespace or one of `"'<>,}])` and backtick. A quoted value (`"…"` or
`'…'`) and a bracketed value (`[ … ]`) are taken whole, including their closing delimiter, so the
delimiter itself is masked rather than leaked. Values longer than 256 characters are not treated as
credentials.

## Where it runs

`Store::remember` redacts before anything else — before the duplicate check and before the id is
derived. Redacting first means an identical credential written twice resolves to the same stored
row, instead of leaving one masked copy beside one unmasked one. Every write path (the `retain`
tool, the MCP server, the importer) goes through `remember`, so none of them can bypass the scan.

## Output

`scan_secret_like_material` returns byte spans, sorted, non-overlapping, each carrying its class.
`redact_secret_like_material` replaces each span with `***` and leaves every other byte alone, so
redaction is idempotent: redacting an already-redacted value changes nothing.
`secret_classes` reports each class once, in first-appearance order.
