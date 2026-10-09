# Task 21 evidence - register LSP orphan process sweep (T11)
Worker st_01a12037 produced the wiring and stalled before committing; the orchestrator verified and committed.
Pinned: oh-my-openagent@adb672532 packages/omo-senpi/src/components/task/process-sweep.ts (wireSessionStartProcessSweep on session_start; skipped for task-child sessions).
Native: crates/maho-cli/src/cli/default_extensions.rs now calls TaskComponent::register_with_process_sweep with maho_omo_task::process_sweep::production_process_sweep_options(env, lsp_daemon::paths::packaged_runtime_defaults(), runtime logger info/warn). maho-cli gains path deps maho-omo-lsp and maho-lsp-daemon.
Verification (orchestrator): cargo nextest run -p maho-cli --test default_extensions --no-fail-fast: 3 passed, 0 skipped, rc 0. cargo clippy -p maho-cli --all-targets -- -D warnings: rc 0, 0 diagnostics.
Cleanup: an orphaned nextest build from the cancelled worker that held a cargo slot for 55 min was killed.