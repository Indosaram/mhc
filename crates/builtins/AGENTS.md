# crates/builtins

## OVERVIEW
Standard system tool definitions, execution sandboxes, and file manipulation primitives.

## WHERE TO LOOK
Each subdirectory is an independent `maho-ext-*` crate; core tools (read/bash/edit/write/grep/find/ls) live in `crates/maho-tools/src/`.

## CONVENTIONS
- Tool arguments must undergo strict JSON schema validation before execution.
- Maintain idempotent behavior on repeated tool invocations where applicable.
- Redact secrets, auth tokens, and sensitive keys from tool execution output.
- All file reads and writes adhere strictly to UTF-8 encoding.

## ANTI-PATTERNS (THIS COMPONENT)
- Avoid unbuffered memory reads of unbounded files (>50MB).
- Do not bypass timeout and cancellation controls on external process runners.
- Never write files without checking parent directory validity.

## NOTES
- Scored 14 due to high file count, distinct capability domain, and system call boundaries.
- Sandboxed execution environments isolate tool side-effects where supported.
