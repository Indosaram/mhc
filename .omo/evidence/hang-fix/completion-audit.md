# Completion audit

Objective: deploy a native mhc build that releases the extension runner mutex before suspended handlers, uses narrow agent field getters for extension contexts, and hides component diagnostics throughout interactive startup, turns, and shutdown. Commit the verified lane, merge hg-final, and clean owned QA processes.

| Requirement | Artifact or command | Current evidence |
|---|---|---|
| Terminate two hung processes | recovery.md; original TERM/GONE observations | Done earlier; PIDs 2232747 and 2261145 |
| Finish warning investigation | harness-smoke2/terminal-ansi.txt | Reproduced actual ulw-loop warning before interactive entry |
| Pending handler does not block renderer snapshot | crates/maho-core/tests/extension_lock_release.rs; green-result.md | Fixed run: 1 passed, 0 skipped, exit 0 |
| Same test fails on pre-fix code | red.log; hg-hang-red at 0dec5825 | Exit 100, 1 failed, 0 skipped; 2.056 seconds; exact cause renderer snapshot blocked while handler was pending |
| Narrow context model getters | maho-agent/src/agent.rs; maho-core/src/agent_session.rs | Five field accessors and nine call sites; get_model routes to session.model |
| Five-crate nextest, zero failures | nextest.log | Exit 0; 2810 passed, 0 skipped, 39.023 seconds across 191 binaries |
| Five-crate all-target clippy -D warnings | clippy.log | Exit 0; finished dev profile in 5m 44s |
| Final CLI startup patch tests and lint | nextest.log; clippy.log | Covered by aggregate gate compiled after final CLI edits; redundant queued CLI gate cancelled |
| Real interactive read tool plus responsive next turn | pty/hang-fix-pty-qa-receipt.json; pty/terminal-ansi.txt | Passed; provider observed fixture read result, then 2 assistant sentinels in terminal |
| Actual warning in hidden debug log, absent from terminal | pty/hidden-debug.log; pty/terminal-ansi.txt | Passed; real ulw-loop warning hidden at startup and turns |
| Workspace release build after final edits | release-final.log; bash_123 exit markers | Exit 0 |
| Package verified | package.log; package/manifest.json; verify-only command | Stage exit 0; 345 files verified before and after installation |
| Install ~/.local/share/mhc and ~/.local/bin/mhc | deployment.md | Deployed; previous installation retained as backup |
| doctor PASS, installed SHA256 | installed-doctor.log; deployment.md | Exit 0, native executable PASS; existing architect credential warning recorded; hash e3ee2d468eeb31e7ab1f974ecb73471492d3a92c5e5615228bd345d14eaabb82 |
| Lane commit and hg-final merge | git log and merge receipt | Pending |
| QA cleanup | PTY receipts; cleanup.md | PTYs/providers/temp HOMEs cleaned; aggregate and baseline commands exited; final tail subscription stopped |

Parity proof: crate audits checked core 69, agent 97 and CLI 141 files. Correct scoped audits after fragment formatting checked agent-session.ts (1 file), agent.ts (1 file), main.ts (2 files). The earlier fully qualified --only arguments checked zero files and are explicitly not used as evidence.

An absent artifact or pending command is not a pass. The package manifest covers file integrity only; it cannot replace runtime PTY or lock regression evidence.

Separate installed interactive startup capture also passed: `startup-only/hang-fix-pty-qa-receipt.json`, binary `/home/indo/.local/bin/mhc`, component warning in hidden log and absent from raw terminal. Its PTY, provider and temporary HOME are all cleaned. This separately satisfies startup inspection rather than relying only on the tool-call scenario.
