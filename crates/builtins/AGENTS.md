# crates/builtins

## OVERVIEW
Standard system tool definitions, execution sandboxes, and file manipulation primitives.

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| Shell execution | `src/bash/` | PTY-backed command runners, timeouts, and signal handling |
| File editing & replacement | `src/edit/` | Exact string replacement matching engine |
| File writing | `src/write/` | Atomic file write and parent directory creation |
| Search tools | `src/grep/`, `src/read/` | Fast file inspection, chunk reading, and regex search |
| Web browsing & fetch | `src/webfetch/`, `src/web_search/` | Network request fetchers and search engine adapters |
| User interaction | `src/ask_user_question/` | Multi-select and choice input dialog primitives |
| AST search & replace | `src/ast_grep_search/`, `src/ast_grep_replace/` | AST-aware code querying and refactoring |

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
