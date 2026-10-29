# Git Bash command runner over stdio MCP — behavior spec (Phase 5)

Status: spec for a new MIT crate (proposed `crates/maho-git-bash`, binary `mhc-git-bash`) that
replaces the SUL `git-bash-mcp` crate and its sibling binary `omo-git-bash`. JSON-RPC framing,
the stdio transport and the client side are already specified in [mcp.md](mcp.md) §1 (the MIT
profile reuses `maho-ext-mcp`); this spec covers only the server's tools, the Windows bash
lookup, the command runner and the process lifecycle. Messages are to be written fresh; only the
facts listed here are mandated.

## 1. Purpose

On native Windows the agent's ordinary shell tool runs `cmd`/PowerShell. Users who have Git for
Windows installed want POSIX shell commands to run through Git Bash instead. The server exposes a
small MCP tool family that locates Git Bash's `bash.exe` and runs commands through it. On every
other platform it offers nothing useful and does not serve.

## 2. Binary and command line

- `mhc-git-bash` (Windows: `mhc-git-bash.exe`) is shipped next to `mhc` as a sibling executable.
- Arguments: none, or the single subcommand `mcp` → serve MCP on stdin/stdout.
- Any other first argument → a one-line usage message on stderr, exit code 2, nothing on stdout.
- A transport I/O failure → the error on stderr, exit code 1. Normal end → exit code 0.
- Server identity reported by `initialize`: name `git_bash`, a version string of the
  implementer's choosing.

## 3. When the server serves

"Available" means: the host is native Windows **and** the lookup in §4 finds a `bash.exe`.

- Not available → the process exits 0 immediately without reading stdin or writing stdout
  (an MCP client sees the server close before `initialize`).
- Available → serve newline-delimited JSON-RPC until stdin reaches end of file or the parent
  process exits (parent-exit detection as in the shared stdio transport). There is **no idle
  timeout**: a long gap between requests must not end the server.

## 4. Locating Git Bash (Windows)

Lookup order; every path examined is appended, in order, to a `checkedPaths` list:

1. Environment variable `OMO_CODEX_GIT_BASH_PATH` (interface fact; users already set it). When it
   is set to a non-blank value (trimmed), it is the **only** candidate: found when it ends in
   `bash.exe` (case-insensitive) and exists; otherwise the lookup fails without trying further
   steps.
2. `C:\Program Files\Git\bin\bash.exe`, source `program-files`.
3. `C:\Program Files (x86)\Git\bin\bash.exe`, source `program-files-x86`.
4. Each non-blank line printed by `where bash` (failure of `where` = no lines), source `path`.
   A candidate whose path (with `/` treated as `\`, case-insensitive) lies under
   `\windows\system32\` or `\microsoft\windowsapps\` is recorded but skipped (those are WSL
   launchers, not Git Bash). Otherwise found when it ends in `bash.exe` and exists.

Source values (interface facts): `env`, `program-files`, `program-files-x86`, `path`, and
`not-required` (non-Windows hosts: found, no path, empty `checkedPaths`).

On failure the result carries `found: false`, `checkedPaths` and an `installHint` text (own
wording) that carries these facts: no usable `bash.exe` was found; the install command
`winget install --id Git.Git -e --source winget`; the variable name `OMO_CODEX_GIT_BASH_PATH`
and that its value is the full path of a `bash.exe`.

A neutral alias variable (e.g. `MHC_GIT_BASH_PATH`, checked first) may be added; the legacy name
must keep working.

## 5. Tools

Tool list order: `run` (only when available, §3), then `which_bash`, then `diagnose`. All input
schemas are JSON objects with `additionalProperties: false`. Descriptions are new prose.

| tool | params | result text (pretty-printed JSON, 2-space indent) |
| --- | --- | --- |
| `which_bash` | none | `{found: true, path, source, checkedPaths}` or `{found: false, checkedPaths, installHint}` |
| `diagnose` | none | `{platform, enabled, status, resolution}` where `resolution` is the `which_bash` object |
| `run` | `command` (string, required), `timeout` (integer 1..1800000 ms), `workdir` (string), `description` (string, informational only) | `{exitCode, stdout, stderr, timedOut}` |

`diagnose`: `platform` is the Node-style platform name (`win32`, `linux`, `darwin`, ...);
`enabled` is true only when available; `status` is `ready` (available), `missing-git-bash`
(Windows, lookup failed) or, on non-Windows hosts, a string whose first word is `disabled`
followed by a short reason of the implementer's wording.

Every tool reply is an MCP `tools/call` result with one text content item and an `isError` flag.
`which_bash` and `diagnose` always reply with `isError: false`, even when Git Bash is missing.

### 5.1 `run` argument rules (checked in this order, each failure → `isError: true`)

1. Not native Windows → error saying `run` is Windows-only.
2. `command` missing, not a string, or blank after trimming → error naming `command`. The trimmed
   text is what runs.
3. Working directory: `workdir`, or the alias `cwd` when `workdir` is absent/null. Present but
   not a non-blank string → error naming `workdir`. Absent → the server's own cwd.
4. Timeout: `timeout`, or the alias `timeout_ms` when `timeout` is absent/null. Accepts a JSON
   number or a numeric string; must be an integer in 1..1800000 → otherwise an error that states
   the range. Absent → the default (§5.2).
5. Lookup fails → `isError: true` with the `which_bash` failure JSON as text.
6. The process cannot be started → `isError: true` with the system error text.

Unknown tool name → `isError: true` with a message naming the tool. Missing or non-object
`arguments` are treated as `{}`.

### 5.2 Default timeout

First valid value (integer, 1..1800000) of: a value configured by the embedding host, then the
environment variables `OMO_CODEX_GIT_BASH_TIMEOUT_MS`, `OMO_CODEX_EXEC_COMMAND_TIMEOUT_MS`,
`CODEX_EXEC_COMMAND_TIMEOUT_MS`, `EXEC_COMMAND_TIMEOUT_MS` (interface facts, in that order;
invalid values are skipped); else 120000. The `run` schema's `timeout` description mentions the
effective default.

### 5.3 Running a command

- Spawn `<bash.exe> -lc <command>` in the chosen working directory, stdin closed, the server's
  environment passed through, no console window opened (`CREATE_NO_WINDOW`).
- stdout and stderr each go to a file in a fresh per-call temporary directory, which is removed
  after the call. Output of any size (test 10 uses more than 1 MiB) must be returned whole
  without the child stalling.
- Output is decoded as UTF-8 with invalid sequences replaced.
- On reaching the timeout the child is killed; `timedOut: true`, and whatever output was written
  so far is returned. `exitCode` is the process exit code, or `null` when none is available.
- A non-zero exit code is **not** a tool error (`isError: false`); the caller reads `exitCode`.

## 6. Protocol surface

- `initialize` → `capabilities.tools.listChanged: false`, `serverInfo`, and `protocolVersion`
  echoing the client's `params.protocolVersion` (default `2024-11-05` when absent).
- `tools/list`, `tools/call` as above; `notifications/initialized` → no reply.
- Any other method → JSON-RPC error `-32601`.
- A request that is not a JSON object → error `-32600` with id `null`.
- A line that is not valid JSON → error `-32601` with id `null` (kept for compatibility with
  existing clients); the server keeps serving.

## 7. Platform notes

- Windows only in effect; on Linux/macOS the binary builds and runs but exits at once (§3).
- Paths are passed to `bash.exe` unchanged; no MSYS path conversion is done by the server.
- Registration with the agent (which MCP config entry launches the binary) is the packaging
  step's concern; the server itself reads no config files.

## 8. Acceptance tests (written from this spec)

Use an injectable platform, environment, file-exists probe, `where` output and command runner so
tests run on any host; the runner tests marked (W) need a real Windows host with Git Bash.

1. CLI: `mcp` and no argument serve; `foo` → usage on stderr, exit 2.
2. Non-Windows: the server exits 0 without output; `diagnose` (called through the handler) reports
   `enabled: false` and a `disabled...` status; `tools/list` has only `which_bash`, `diagnose`.
3. Windows + Git Bash: `tools/list` is `run`, `which_bash`, `diagnose` in that order; a long idle
   gap does not stop the server; stdin EOF ends it with exit 0.
4. Lookup: env var valid → source `env`; env var set to a non-existent or non-`bash.exe` path →
   missing, no fallback to Program Files, `checkedPaths` = that one path; Program Files first,
   then x86; `where` output with a `System32` and a `WindowsApps` entry before a Git one → both
   recorded in `checkedPaths`, Git one chosen with source `path`.
5. Missing Git Bash: `installHint` contains the winget command and the env var name;
   `diagnose.status` = `missing-git-bash`; `run` → `isError: true` with that JSON.
6. `run` validation: blank `command`; `workdir: ""`; `workdir: 5`; `timeout: 0`, `1800001`,
   `1.5`, `"abc"` → each `isError: true`; `timeout: "500"` accepted as 500; `cwd` and
   `timeout_ms` aliases honoured; `workdir` wins over `cwd`.
7. Default timeout: host value beats env vars; env vars in the listed order; an invalid earlier
   variable is skipped; nothing set → 120000.
8. Runner result mapping: exit 3 with output → `isError: false`, `exitCode: 3`; runner error →
   `isError: true` with its text; unknown tool → `isError: true`.
9. Protocol: `initialize` echoes `protocolVersion` or defaults to `2024-11-05`; unknown method →
   `-32601`; array request → `-32600`; garbage line → `-32601` with null id, next request still
   answered; `notifications/initialized` produces no output.
10. (W) `echo hi; echo err >&2` → stdout `hi\n`, stderr `err\n`; `sleep 5` with `timeout: 200`
    → `timedOut: true` within a bounded wait; `pwd` honours `workdir`; output above 1 MiB is
    returned whole; the per-call temp directory no longer exists afterwards.
