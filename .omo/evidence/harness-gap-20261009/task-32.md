# Installed acceptance V01-V07

Verdict: confirmed on the gate57 staged package, mhc SHA256
f37621dec36026e2dd9e5d255d65e6a45068ffdb32c61f64424c0f068a7f83a8.

| Surface | Evidence | Result |
|---|---|---|
| Native tasks and workpool | acceptance-final-task/qa/hg-v-task.json | exit 0 |
| Isolation | acceptance-final-isolation/qa | exit 0 |
| ULW entry and state | acceptance-final-ulw/qa | exit 0 |
| JS/Python eval, reload monitor, cleanup | acceptance-ready-eval/qa/hg-v-eval.json | exit 0 |
| MCP discovery and invocation | acceptance-final-mcp/qa | exit 0 |
| Memory | acceptance-final-memory/qa | exit 0 |
| LSP diagnostics, symbols, rename | acceptance-ready-lsp/qa/hg-v-lsp.json | exit 0 |
| Auth and secret-free diagnostics | acceptance-final-auth.log, acceptance-final-doctor.log | exit 0 |
| RPC sessions, CLI fork, messages | acceptance-final-rpc-session*.log | exit 0 |

Drivers ran independently in parallel. Initial eval/LSP connection failures
are preserved, not overwritten; corrected readiness drivers passed against
the same package hash. See gate57.md for the bind/listen readiness correction.

Cleanup receipts in each driver artifact cover owned host and descendant
processes, sockets and temporary HOME/workspace removal. Eval records pidfd
subscriptions armed before termination for both kernels and monitor; LSP
records daemon and server descendant exits. No live-desktop stack was installed
to force the accepted 23 environment-dependent tests.
