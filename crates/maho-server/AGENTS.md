# crates/maho-server

## OVERVIEW
Shared-host daemon protocols, clients, and persistent session SQLite storage.

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| RPC protocol definitions | `src/protocol/` | Message frames, serialization, and error schemas |
| SQLite database storage | `src/sqlite/` | Session persistence, message logs, migrations |
| Client SDK bindings | `src/client/` | In-process and network RPC client utilities |
| Server lifecycle | `src/server/` | Daemon startup, shutdown, and socket binding |
| Top-level exports | `src/lib.rs` | Public interface and re-exports |

## CONVENTIONS
- Database transactions must ensure ACID guarantees across multi-row state mutations.
- Keep protocol schema definitions backward-compatible with older clients.
- Workspace lints enforced: zero unwrap across server logic.
- Daemon frames follow structured framing protocols matching upstream spec.
- Connection states cleanly deregister upon TCP / socket teardown.

## ANTI-PATTERNS (THIS COMPONENT)
- Avoid unbounded memory storage of active connection buffers.
- Never run long blocking operations directly inside the async server dispatcher.
- Do not bypass database migrations when altering session table schemas.
- Avoid raw unhandled socket panics on client disconnections.

## NOTES
- Scored 15 due to high file count, database persistence boundary, and RPC server centrality.
- Concurrency tests run with isolated temporary SQLite databases per test case.
