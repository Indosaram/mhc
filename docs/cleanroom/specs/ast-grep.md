# Structural code search and rewrite — behavior spec (Phase 4/5)

Scope: three agent tools that search, rewrite and rule-scan source code by syntax tree, backed by
the external `ast-grep` command-line program. They replace `crates/omo/ast-grep-mcp` and
`crates/omo/components/maho-omo-ast-grep` (see `phase4-5-inventory.md`). The spec writer read
those sources for understanding only. It was written from them, from the public ast-grep CLI
documentation (`ast-grep run`, `ast-grep scan`, `--json=stream`, `--update-all`) and from the MCP
protocol specification. Proposed crate name: `maho-ast-grep`.

## 1. Shape

- The tools come from a stdio MCP server that the MIT profile registers lazily: the process is
  started on the first call, and the server is exposed through the tool-search catalog instead of
  being always loaded. Server name (interface fact): `ast_grep`. Tool names (interface facts):
  `search`, `rewrite`, `scan`. A host may prefix them, as other MCP tools are prefixed.
- The server binary sits next to the mhc executable (`<name>` or `<name>.exe`) and is started with
  the single argument `mcp`. Any other first argument prints a usage line to stderr and exits with
  code 2. If the binary is missing, or the path is a directory, nothing is registered and one
  diagnostic line naming the missing path is emitted. The session goes on without these tools.
- The registering session passes its working directory to the server in an environment variable.
  An explicit value already in the environment wins. The server uses it as the default `workdir`.
  Proposed name: `MHC_AST_GREP_PROJECT_CWD` (the old `OMO_AST_GREP_PROJECT_CWD` is also read).
- No `omo.jsonc` keys. The idle timeout of the server can be set with an environment variable (see
  section 7).

## 2. Finding the `ast-grep` program

The program is resolved for each tool call. A successful result may be cached for the life of the
process, keyed by platform, architecture and the candidate list. A cached path is dropped when its
file disappears. Candidates are tried in this order, and the first one that passes wins:

1. the environment override `OMO_AST_GREP_SG_PATH` (interface fact; trimmed; empty is ignored);
2. a managed install under `~/.omo/runtime/ast-grep/<platform>-<arch>/` (on-disk path, interface
   fact);
3. `ast-grep`, then `sg`, looked up on `PATH` (with `.exe` on Windows);
4. the standard Homebrew bin directories of the platform (interface fact: these are the
   locations Homebrew documents for macOS on Apple silicon and Intel, and for Linuxbrew).

A candidate passes when it is a non-empty file and the output of `<candidate> --version` contains
`ast-grep` (case-insensitive). The `sg` name on Linux is often the shadow-group tool, so the
probe is required. When nothing passes, the call fails with `BINARY_NOT_FOUND`. The message names
the platform, and `details.hints` lists install commands for that platform (Homebrew, npm
`@ast-grep/cli`, cargo, or scoop/winget/choco on Windows).

## 3. Tool parameters

All tools reject unknown properties and non-object input. Lengths marked "bytes" count UTF-8
bytes. All other length limits count characters (code points).

Common to all three tools:

| key | type | rule | default |
| --- | --- | --- | --- |
| `paths` | string[] | required, 1-64 items, each 1-4096 chars; never defaulted | — |
| `workdir` | string | 1-4096 chars; cwd of the child process | project cwd (section 1), else server cwd |
| `globs` | string[] | 0-32 items, each 1-1024 chars; passed through as include/exclude globs | none |
| `maxMatches` | integer | 1-500 | 50 |
| `timeoutMs` | integer | 1000-300000; budget for the whole call | 300000 |
| `includeHidden` | boolean | also search hidden files | false |
| `followSymlinks` | boolean | follow symbolic links | false |

- `search` adds: `pattern` (required, 1 char to 16 KiB bytes), `language` (required, one of the
  25 names below), `selector` (1-128 chars), `strictness` (`cst|smart|ast|relaxed|signature`,
  default `smart`) and `force` (boolean).
- `rewrite` adds the `search` keys plus `rewrite` (required string, up to 64 KiB bytes; the empty
  string is allowed and removes the matched code) and `apply` (boolean, default false = dry run).
- `scan` adds exactly one of `ruleFile` (1-4096 chars, path of a YAML rule file) and
  `inlineRules` (1 char to 64 KiB bytes of YAML rule text), plus `includeMetadata` (boolean) and
  `apply` (boolean, default false). Only the given rule source is used; an `sgconfig.yml` found in
  the tree must not change the result.
- Languages (interface fact): bash, c, cpp, csharp, css, elixir, go, haskell, html, java,
  javascript, json, kotlin, lua, nix, php, python, ruby, rust, scala, solidity, swift,
  typescript, tsx, yaml.
- The published input schemas contain no `oneOf`/`anyOf`/`allOf`/`not`, at any depth. The scan
  rule-source exclusivity is checked at run time. Byte limits are stated in the descriptions,
  because JSON Schema `maxLength` counts characters.

## 4. Pattern preflight (`search`, `rewrite`)

Before the program is started, the pattern is checked for common mistakes:

- Always rejected (`INVALID_ARGUMENT`, or `UNSUPPORTED_LANGUAGE` for a bad language): a
  whitespace-only pattern, an unknown language, empty paths, a non-positive limit.
- Rejected unless `force: true` (`PATTERN_HINT_REJECTED`). At minimum these must be caught;
  the implementer chooses how to detect them and may add more:
  - metavariable misuse: exactly two leading dollars, or a name containing lower-case letters
    (the wildcard `$_` stays valid);
  - regular-expression syntax instead of code: backslash escape classes, a dot followed by `*` or
    `+` (not when it directly follows a `$` metavariable), and a bracketed character range;
  - a function or class head that cannot parse on its own in rust, go, python or the
    javascript family (head without a body, or a python head ending in a colon).
- Warning only: a single `|` between operands outside string literals (`||` is fine).
- `rewrite` only, always rejected even with `force`: the replacement uses a metavariable that the
  pattern does not capture (`REWRITE_UNBOUND_METAVARIABLE`), or uses a name as single (`$X`) where
  the pattern captures it as multi (`$$$X`), or the reverse (`REWRITE_METAVARIABLE_KIND_MISMATCH`).
- On rejection, the error message is the first rejecting finding, and all findings are listed in
  `details`. Warnings that do not reject are added to the success `warnings`.

## 5. Execution

- `search` runs `ast-grep run -p <pattern> --lang <language> --json=stream --strictness <s>
  [--selector <sel>] [--globs <g>]... [--no-ignore hidden] [--follow] <paths>...` in `workdir`.
  stdin is closed, and no console window is shown on Windows.
- `rewrite` runs a preview with the same arguments plus `-r <rewrite>`. If `apply` is set, it
  then runs a second process with `--update-all` instead of `--json`. `scan` works the same way
  with `ast-grep scan (--rule <file> | --inline-rules <text>) [--include-metadata]`.
- Output is read as one JSON object per line, under these bounds:
  - A single line over 1 MiB fails with `OUTPUT_TOO_LARGE`. Invalid UTF-8 fails with
    `ENCODING_ERROR`.
  - Collection stops at `maxMatches` (reason `match_limit`), or when the serialized records would
    pass 4 MiB (reason `output_cap`). The child is then terminated.
  - Lines that are not JSON objects are skipped. If no record parsed, the call fails with
    `OUTPUT_PARSE_FAILED`. If some parsed, the result is marked truncated with reason
    `sg_output_truncated`.
  - stderr is kept up to 64 KiB (cut at a character boundary).
- Exit 0 or 1 is normal (1 = no matches). Any other exit code, or exit 1 with a stderr line that
  starts with `ERROR`/`error:`, fails with `SG_FAILED`. A spawn failure also fails with
  `SG_FAILED`.
- If the deadline passes, the call fails with `TIMEOUT`. On abort (section 7) it fails with
  `ABORTED`. In both cases the child gets a polite terminate signal first, and a hard kill about
  1 s later.
- If stderr says that the pattern contains an error node, the call fails with
  `PATTERN_PARSE_FAILED`. This is an error result, never an empty `ok: true` result.
- `scan`: if stderr shows that the rule could not be parsed, the call fails with
  `RULE_PARSE_FAILED`. A deprecation warning on stderr is copied into `warnings`.
- Apply safety (`rewrite` and `scan`):
  - A truncated preview is never applied (`PREVIEW_TRUNCATED`).
  - A preview with zero matches returns success with `applied: false` and a warning.
  - The second pass gets only the time left of `timeoutMs`. If none is left, the call fails with
    `TIMEOUT` and the second pass is not started.
  - For `rewrite`, a second pass that exits 1 (it found nothing) fails with
    `REWRITE_STALE_PREVIEW`.
  - On success the counts are the preview counts, and a warning says so.

## 6. Results

Each tool returns one text content item: a JSON object with `schemaVersion: 1`. The MCP `isError`
flag is set exactly when `ok` is not `true`.

- Match: `path` is relative to `workdir` with `/` separators. A path outside `workdir` stays
  absolute; on Windows it is `C:/...` and compared case-insensitively. A match also has
  `language` (lower case), `text`, `range.start` / `range.end` with `{line (1-based), column
  (0-based), byteOffset}`, and `metavariables {single: {NAME: text}, multi: {NAME: text}}`. A
  multi capture is the source text from the first captured node to the last; an empty capture is
  `""`. Other fields of the record are passed through. Matches are sorted by path, then by start
  byte offset.
- `rewrite` and `scan` matches always carry `replacement` (`""` if absent). A `scan` match moves
  `ruleId`, `severity`, `note`, `message`, `labels` (default `[]`) and `metadata` into a `rule`
  object.
- `search` success: `{ok: true, kind: "search", workdir, counts: {returnedMatches, returnedFiles,
  totalMatches, totalFiles, atLeastMatches}, matches, truncation, warnings, durationMs}`. The
  totals are `null` when truncated. `atLeastMatches` is the count plus one when a limit cut the
  output.
- `rewrite`/`scan` success: `{ok: true, kind, workdir, applied, matches, counts: {plannedMatches,
  plannedFiles}, truncation, application: {requested, performed, countsArePreviewBased: true,
  secondPassExitCode}, warnings, durationMs}`. `rewrite` also reports
  `application.idempotencyChecked: false`.
- `truncation`: `{truncated, reason (match_limit|output_cap|sg_output_truncated|null),
  maxMatches, maxPayloadBytes, salvagedRecords}`. A truncated `search` also carries a warning that
  says the limit was reached and suggests reducing the scope.
- Failure: `{ok: false, kind?, error: {code, message, retryable, phase
  (preflight|search|preview|apply), language?, details}, durationMs?}`. `retryable` is true only
  for `TIMEOUT`, `ABORTED`, `OUTPUT_PARSE_FAILED` and `REWRITE_STALE_PREVIEW`.
- Error codes (interface fact, 19): `INVALID_ARGUMENT`, `BINARY_NOT_FOUND`, `BINARY_INVALID`,
  `UNSUPPORTED_LANGUAGE`, `PATTERN_HINT_REJECTED`, `PATTERN_PARSE_FAILED`, `RULE_PARSE_FAILED`,
  `REWRITE_UNBOUND_METAVARIABLE`, `REWRITE_METAVARIABLE_KIND_MISMATCH`, `PATH_NOT_FOUND`,
  `PATH_UNREADABLE`, `TIMEOUT`, `ABORTED`, `OUTPUT_TOO_LARGE`, `OUTPUT_PARSE_FAILED`,
  `PREVIEW_TRUNCATED`, `REWRITE_STALE_PREVIEW`, `SG_FAILED`, `ENCODING_ERROR`.
- An unknown tool name returns `INVALID_ARGUMENT` (phase `preflight`) that lists the available
  tools. An unexpected internal failure becomes `SG_FAILED`.

## 7. Server protocol and lifecycle

- Newline-delimited JSON-RPC 2.0 on stdio. Requests are handled as follows:
  - `initialize` returns `serverInfo {name: "ast_grep", version}` and capabilities with
    `tools.listChanged: false`. It echoes the client's `protocolVersion`, or `2024-11-05` when the
    client sent none.
  - `ping` returns `{}`.
  - `notifications/initialized` gets no reply.
  - `tools/list` returns the three tools.
  - `tools/call` without `params.name` is an error (-32602). An unknown method is an error
    (-32601), and so is a non-object request (-32600). A line that is not JSON is an error
    (-32700).
- The server still answers the protocol requests when the `ast-grep` program cannot be found. Only
  tool calls fail.
- Every call gets its own cancellation signal. When the parent process exits, the call in
  progress is aborted, and the server exits when stdin closes.
- Idle timeout: the server exits after 10 minutes without requests. The environment variable
  `MHC_AST_GREP_IDLE_TIMEOUT_MS` changes this (the old `OMO_AST_GREP_IDLE_TIMEOUT_MS` is also
  read). Zero or a negative value disables the timeout. An empty, non-numeric or non-finite value
  keeps the default.

## 8. Acceptance tests

Tests that need a real `ast-grep` must fail when it is missing; they must not skip.

1. Registration: if the server binary exists, one lazy stdio server `ast_grep` is registered with
   argument `mcp`, and the project cwd is passed in the environment. If the binary is missing or
   is a directory, nothing is registered and one diagnostic is emitted.
2. An explicit project-cwd environment value is kept and not overwritten by the session cwd.
3. `initialize` with no version returns `2024-11-05`. A newer version from the client is echoed.
4. `tools/list` returns exactly `search`, `rewrite`, `scan`, and no schema contains a composition
   keyword at any depth.
5. `ping` returns `{}`; `notifications/initialized` gets no reply; a non-object request returns
   -32600, an unknown method -32601, `tools/call` without a name -32602, a broken line -32700.
6. With no `ast-grep` program, `initialize` still works and `search` fails with
   `BINARY_NOT_FOUND`, with non-empty install hints.
7. Resolver: an override that points to a program whose `--version` output lacks `ast-grep` is
   skipped, and the next candidate on `PATH` is used.
8. `search` with `{}` fails with `INVALID_ARGUMENT` in phase `preflight`. So does an unknown
   property, and so does a pattern of 16385 bytes made of multi-byte characters, even when it has
   fewer characters than that.
9. `search` for `alert($TEXT)` in javascript over a temp tree with two files returns relative
   paths, 1-based lines, `metavariables.single.TEXT`, and matches sorted by path and then offset.
10. A `$$$ITEMS` capture over three call arguments returns the exact source slice, commas
    included.
11. A file outside `workdir` keeps an absolute path. A Windows-style workdir and file give a
    relative path even when the letter case differs.
12. The pattern `\bsize` is rejected with `PATTERN_HINT_REJECTED`. With `force: true` it runs.
    `len($value)` is rejected. `len($_)` is accepted.
13. A python `class $K($$$):` pattern is rejected. Without the colon it is accepted.
14. A pattern that ast-grep reports as an error node fails with `PATTERN_PARSE_FAILED`, not with
    zero matches.
15. With `maxMatches: 2` and five matching lines: two matches, `truncated: true`, reason
    `match_limit`, null totals, `atLeastMatches: 3`, and a narrowing warning.
16. A fake program that writes one JSON record and then garbage gives one match with reason
    `sg_output_truncated` and `salvagedRecords: 1`. Only garbage gives `OUTPUT_PARSE_FAILED`.
    A line over 1 MiB gives `OUTPUT_TOO_LARGE`.
17. A fake program that exits 2 gives `SG_FAILED`. Exit 1 with an `error:` stderr line gives
    `SG_FAILED`. Exit 1 with an empty stdout gives zero matches and `ok: true`.
18. A fake program that hangs, with `timeoutMs: 1000`, gives `TIMEOUT` (retryable), and the child
    is gone afterwards.
19. Closing the parent while a call hangs aborts that call (`ABORTED`). The next call gets a fresh,
    unaborted signal.
20. `rewrite` without `apply` leaves the files unchanged and returns `replacement`s, planned counts
    and `applied: false`.
21. `rewrite` with `apply: true` changes the files, sets `applied: true`, reports preview-based
    counts and a non-null `secondPassExitCode`.
22. A `rewrite` that uses `$Y`, where the pattern only binds `$X`, fails with
    `REWRITE_UNBOUND_METAVARIABLE`, even with `force`. `$X` against a pattern `$$$X` fails with
    `REWRITE_METAVARIABLE_KIND_MISMATCH`.
23. `apply` on a truncated preview fails with `PREVIEW_TRUNCATED`, and the files are unchanged.
    `apply` with zero matches succeeds with `applied: false` and a warning.
24. If the files change between preview and apply, so that the second pass exits 1, the call fails
    with `REWRITE_STALE_PREVIEW` (retryable).
25. `scan` with both `ruleFile` and `inlineRules`, or with neither, fails with `INVALID_ARGUMENT`.
    Invalid YAML fails with `RULE_PARSE_FAILED`.
26. `scan` with an inline rule that has a `fix` returns matches with `rule.ruleId`, `labels: []`
    and `replacement`. With `apply: true` the files are rewritten.
27. Idle timeout: an unset value or `soon` means 600000 ms. `-5` disables the timeout.
