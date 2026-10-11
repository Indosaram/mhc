# Gate 57

Product source: f06734eac917feef66dee57c9c0a4150b184031e.
Tree: dfeb88925d81a63b1bdb344bac129e1dd7d3fe6e.
Start and end commit identical, zero dirty tracked files.

Workspace nextest: 20,387 passed, 23 accepted live-desktop environment N/A skips.
Strict workspace all-target clippy: exit 0.
Locked workspace release binaries: exit 0.
Packaging stage and manifest verification: exit 0.
Package self-test: 91/91 passed. All-crate parity audit: 138 PASS, zero FAIL.
Staged mhc SHA256: f37621dec36026e2dd9e5d255d65e6a45068ffdb32c61f64424c0f068a7f83a8.

Initial parallel installed acceptance: auth, doctor, ulw, isolation, memory,
task, MCP and RPC session normal/fork/messages passed. Eval JS and monitor,
and LSP rename failed with Failed to connect before any provider request.
Python eval passed. These failed attempts are preserved in acceptance-final-*.

The eval and LSP drivers subscribed before spawn but treated Unix socket
creation as listening readiness. Unix bind creates the path before listen;
the host sets mode 0600 after Tokio UnixListener::bind returns. The corrected
drivers await that post-bind metadata event, without sleeps or connection
retries. Fresh acceptance-ready-eval and acceptance-ready-lsp results are
passed with exit 0. Eval JS, Python, monitor and child cleanup all passed;
LSP diagnostics, symbols and rename all passed. This change modifies acceptance drivers only; product source and
staged binary are unchanged.

Independent reviewer st_01a12b56 confirmed F2 and F4 for the final reload fix;
F1 installed acceptance is now confirmed; publish/deploy receipts remain pending.
No full 1,412-file line-by-line review is claimed.
