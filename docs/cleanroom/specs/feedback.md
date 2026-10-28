# Feedback and bug reports (Phase 5)

Status: behaviour spec for the MIT profile. Covers the `/feedback` and `/bug` commands, the
report bundle they build, and the two RPCs a desktop or other front end uses for the same flow.
The spec writer read the SUL component to learn its behaviour. The implementer must use this file
and MIT sources only.

## 1. What it is

A user can send a short report from inside a session. Nothing is uploaded until the user has
had the chance to inspect a **draft**: a fixed list of sections whose text is byte-for-byte the
upload, with secrets already stripped. Sending posts the draft to a report intake service; with
uploads disabled it is written to a private local file instead.

## 2. Surface

| Kind | Name | Input | Notes |
| --- | --- | --- | --- |
| command | `/feedback` | optional free text | report kind `feedback`; has a short description and argument hint (own wording) |
| command | `/bug` | optional free text | report kind `bug`; has a short description and argument hint (own wording) |
| RPC | `omo.feedback.prepare` | JSON object (see 5.1) | returns a draft or `{kind:"unavailable", reason}` |
| RPC | `omo.feedback.submit` | JSON object (see 5.2) | returns a submit result |

The RPC method names, the JSON field names, the section ids and the result/refusal values in this
file are interface facts (front ends and the intake depend on them).

Environment (interface facts):

- `OMO_DISABLE_FEEDBACK`: uploads are off when it is set to anything except empty, `0`, `false`,
  `no`, `off` (trimmed, case-insensitive).
- `OMO_SERVICE_API_URL`: intake origin; default `https://api.omo.dev`. Reports are POSTed to
  `<origin without trailing slash>/v1/reports`.
- `OMO_NATIVE_SURFACE=desktop` marks the client app as `desktop`, otherwise `cli`.

No configuration keys. No agent tool is registered.

## 3. Prerequisite: the upload redactor

The unit needs one host-provided **upload redactor**: a function from text to (cleaned text,
number of replacements). Running it on its own output must change nothing. Without one, both commands only show a warning
that feedback cannot be used in this build, `prepare` returns `unavailable` with a reason, and
`submit` returns refused `unavailable`. Nothing is collected or sent.

## 4. The draft

### 4.1 Sections (fixed order; wire ids)

`description`, `identity`, `environment`, `session`, `logs`, `crash`, `config`.

A draft lists only the sections that were collected, always in this order, whatever order they
were added in. `description` is always present and is empty in the draft; the description is
filled at submit time. Every section except `identity` goes through the upload redactor. Each
section carries `id`, a human `title` (wording is free), `text`, `redactedCount`, `bytes` (UTF-8
length of the text). The draft also has `draftId` (`drf_` + a random UUID), `destination`
(`intake`, or `local` when uploads are off), `retentionDays` (90), `signedIn` (true when the
prepare input had a string `accessToken`), `totalBytes` and `redactedCount` (sums over sections).

### 4.2 Host-collected sections

- **identity** (not redacted): the device host name. In the MIT profile no anonymous install or
  session hash is added (see telemetry.md; the wire fields for them stay optional and are left
  out).
- **environment**: product version and host version, OS name, kernel release (or `unknown`),
  CPU architecture, locale (first of `LC_ALL`, `LC_MESSAGES`, `LANG` whose part before `.` is
  non-empty and not `C`/`POSIX`, with `_` turned into `-`; else `unknown`), release channel
  (`beta` when the product version contains `-`, else `latest`), report time in RFC 3339 UTC
  with milliseconds, and the time zone (`TZ`, else the local UTC offset).
- **config**: the merged user configuration (as loaded for the session's cwd), pretty-printed
  JSON, after a structural secret pass:
  - a value under a key whose name (normalised to snake case: camelCase and `-` split into `_`)
    ends in a credential-like word is replaced by `<redacted>`. The word set is the
    implementer's choice; it must at least cover API keys, tokens, secrets, passwords,
    auth/authorization values, cookies and credentials (singular and plural). A name that also
    contains a word for a numeric bound or size (for example `max_tokens`, `token_limit`,
    `context_window`) is not secret. A nested object/array under a secret key is blanked leaf by
    leaf;
  - every leaf inside an object under a `headers` or `env` key (any case) is `<redacted>`;
  - userinfo in a URL-looking string (`scheme://user:pass@host`) becomes `scheme://<redacted>@host`.
- **logs** (omitted when empty): files matching `<agent dir>/logs/*.log` (names with any other
  ending are ignored), in name order. Only the last 512 KiB of each file are read (a
  partial first line is dropped). Each line that parses as a JSON object with an RFC 3339 `ts`
  (or `timestamp`) within the last 15 minutes is projected onto a fixed allowlist of fields that
  never hold user content (time, level, event/component/kind names, status and error codes, durations, attempt and
  turn counters, model/provider, session/task/request ids, pid, exit code, signal). A kept field
  must be a number, a boolean, or a single-line string of at most 160 UTF-16 units; anything else,
  and every field not on the list (message bodies, prompts, paths, arguments), is dropped. At most
  the last 200 projected lines per file. Each file becomes a block headed `## <file name>`.
- **session** (only when a session is attached): a header with session id, model
  (`provider/id`) and cwd (`unknown` for missing values), then the transcript: only entries of
  type `message` are used (extension-defined entry types are skipped), one line per message
  `role (HH:MM:SS): body`, the time part omitted when the entry has no parseable timestamp. The
  body is the text parts joined by spaces, tool calls as `[name] <JSON arguments>`, images as
  `[image]`; a body longer than 2400 characters keeps its first and last 1200 characters around a
  marker that says how many were cut; blank lines are removed and continuation lines are
  indented by two spaces. Only the last 50 messages are kept.

### 4.3 Client-supplied additions (RPC only)

- `sections`: entries `{id, text}` are accepted only for `logs` and `crash` with string text;
  others are ignored. A client section with the same id as a host one is placed first, then a
  newline, then the host text.
- `identity`: an object with optional `accountId`, `userId`, `plan`, `deviceId` (must match
  `^[A-Za-z0-9_.:@-]{1,128}$`), `email` (must match `^[^\s@]{1,64}@[^\s@]{1,190}$`; the parts before and after the
  last `@` are given to the redactor separately and each must come back unchanged), `displayName`, `deviceName` (control-character
  runs collapse to one space, trimmed, non-empty, at most 120 UTF-16 units). A value is kept only
  when the upload redactor finds nothing in it. Kept values become label-aligned lines placed
  before the host identity lines, and the identity section stays unredacted.

### 4.4 Draft store

Drafts live in memory per process, for 30 minutes. Preparing a draft first drops expired drafts,
then evicts the oldest while 8 or more are stored. A successful send, a local save, an "already
sent" result, a secret refusal or a too-large refusal removes the draft; after `rate-limited`
or `unavailable` the draft stays stored and may be submitted again.

## 5. RPCs

### 5.1 `omo.feedback.prepare`

Input fields: `sessionId` (absent → the most recently started live session; a string → that live
session, none if unknown; `null` → no session), `accessToken`, `sections`, `identity`. Live
sessions are tracked from session start/shutdown; on shutdown of the latest one, the latest
becomes the most recently added remaining one. Output: the draft JSON from 4.1.

### 5.2 `omo.feedback.submit`

Input: `draftId`, `kind` (`feedback`, `other`; anything else is `bug`), `description`,
`accessToken`, `idempotencyKey`. Output, one of:
`{kind:"sent", reportId}`, `{kind:"saved", path}`, `{kind:"already-sent"}`,
`{kind:"refused", reason}` with reason `secret-detected | too-large | rate-limited | unavailable`.
An unknown or expired draft id is refused `unavailable`.

Submit behaviour:

1. The description is passed through the upload redactor. The report body (JSON) has `kind`,
   `redactorVersion` (`"1"`), `title` (first non-blank description line, trimmed, at most 120
   characters), `client` (`app`, `omo` = product version, `senpi` = host version, `os`, `arch`,
   `locale` from `LANG` as above, `channel`, optional `deviceName`), optional `installHash` and
   `sessionHash`, and `sections` as `[{id, text}]` in draft order with the description text in
   the `description` section. The other sections are not redacted a second time; their text
   equals the draft's.
2. Uploads off: the body is written to `<state dir>/feedback/report-<draftId>.json` and the
   result is `saved` with that path. Write failure → refused `unavailable`.
3. Otherwise POST with headers `content-type: application/json`, `idempotency-key`, and
   `authorization: Bearer <accessToken>` when a token was given. The key is the caller's
   `idempotencyKey` when it matches `^feedback-[A-Za-z0-9_-]{4,96}$`, else `feedback-<draftId>`.
4. Response mapping: network error → `unavailable`; 2xx with a string `id` → `sent`; 2xx without
   one → `unavailable`; 409 with error code `conflict` **and** a caller-supplied valid key →
   `already-sent`; error code `secret_detected` → `secret-detected`; 413 → `too-large`;
   429 → `rate-limited`; anything else → `unavailable`. The error code is read from
   `error.code` of the JSON response.
5. On `sent`, when the response has a string `deleteToken`, append `{id, deleteToken, createdAt}`
   to `<state dir>/feedback/sent.json` (a JSON array; an entry with the same id is replaced;
   unreadable or non-array content starts a fresh array). A failure here does not change the
   result.

`<state dir>` is `<agent dir>/omo-senpi` (existing on-disk location, kept for compatibility).

## 6. Command flow (`/feedback`, `/bug`)

1. Prepare a draft for the current session. A prepare error is shown as an error notification
   and the flow ends.
2. Description = the trimmed argument; when empty, ask with a text input. Cancel or a blank
   answer ends the flow silently.
3. Show a selection whose title states which sections are included (in plain words) and either
   that secrets are removed and how many days the report is kept (intake) or that uploads are
   off via `OMO_DISABLE_FEEDBACK` and the report will be saved locally. Options: send, view the
   exact content, cancel.
4. "View" shows every section except the description as `## <title>` blocks in one info
   notification and returns to the selection. Cancel or dismiss ends silently.
5. "Send" calls submit with `{draftId, kind, description}` and shows one notification: info for
   `sent` (with the report id), `saved` (with the path) and `already-sent`; error for each
   refusal, with a hint per refusal reason saying what the user can do next (wording is free).

## 7. Platform notes

On Unix the `feedback` directory is created with mode `0700` and `report-*.json` / `sent.json`
with mode `0600`. Elsewhere default permissions are used. The kernel release is read from the
OS where available, else `unknown`.

## 8. Acceptance tests

1. Without an upload redactor, `/bug` shows only a warning; `prepare` returns `unavailable` with a
   non-empty reason; `submit` returns refused `unavailable`; no network call is made.
2. Collected sections out of order come out as `description, identity, ..., config` with an empty
   description; `identity` text is unchanged and has `redactedCount` 0; other sections are
   redacted and counts/bytes sum into the draft totals; `retentionDays` is 90.
3. Config pass: `githubToken` → `<redacted>`, `tokenLimit` (a number) kept, every leaf under an
   `Env` object and under `headers.auth.inner` blanked, `ftp://alice:x@srv/a` →
   `ftp://<redacted>@srv/a`, an `accessKey` inside an object inside an array blanked, an
   ordinary string such as `dark` kept.
4. Transcript: a non-`message` entry is skipped; a user message `one\n\ntwo` stamped
   `2025-06-07T22:15:09Z` renders as `user (22:15:09): one` plus `  two`; an assistant message
   with a tool call `grep` with arguments `{"q":1}` followed by text `done` renders as
   `[grep] {"q":1} done`; an entry without a timestamp has no time part; 75 messages keep only
   the last 50.
5. A 4000-character body keeps its first and last 1200 characters and states that 1600 were cut;
   a 2400-character body is unchanged.
6. Logs: a recent line keeps only allowlisted scalar fields (a free-text `msg` field and a
   string field containing a line break are dropped); a line 20 minutes old, a line without a
   timestamp, non-JSON lines and a file named `a.log.old` contribute nothing; a missing logs
   directory gives no section.
7. Client identity: a valid `deviceId` and `email` are kept; an `accountId` containing `/` is
   dropped; a `deviceName` with a tab is collapsed to one line; a `displayName` the redactor
   flags is dropped; a 200-character `deviceName` is dropped. Kept lines appear before the host
   identity lines.
8. Client sections: `logs` and `crash` accepted (and redacted), `session` ignored; `signedIn` is
   true when `accessToken` is a string.
9. Submit success (201 with `id` and `deleteToken`): result `sent`; URL is
   `<origin>/v1/reports` with one slash; headers include `idempotency-key: feedback-<draftId>`
   and the bearer token; body `title` and description section are redacted; `sent.json` holds the
   delete token; the draft is gone and a second submit is refused `unavailable`.
10. A valid caller key is sent as-is and a 409 `conflict` gives `already-sent`; an invalid key is
    ignored (the draft key is used) and the same 409 gives `unavailable`.
11. Refusal mapping: `secret_detected` code, 413, 429, 500 with a non-JSON body map to the four
    reasons; after 429 the draft is still stored and can be submitted again.
12. More than 8 prepares leave exactly 8 drafts; a draft older than 30 minutes is refused
    `unavailable`.
13. `OMO_DISABLE_FEEDBACK=1`: draft destination is `local`; submit writes
    `feedback/report-<draftId>.json` under the state dir, makes no request, returns `saved`; on
    Unix the file is `0600` and the directory `0700`. `OMO_DISABLE_FEEDBACK=off` keeps uploads on.
14. Command flow: blank argument → input asked; choose view then send → one info notification
    listing non-description sections, submit called with `{draftId, kind:"bug", description}`,
    one info notification with the report id. Cancel → nothing submitted, no notification.
    Prepare failure → one error notification, nothing submitted.
15. RPC session choice: no `sessionId` uses the latest live session; `null` gives a draft without
    a `session` section; after the latest session shuts down the previous one is used.
