# Monitor reload investigation

Baseline: gate56 commit 045603311878f8867d9711aeedb390cbdf44e142.
Workspace gate: 20,386 passed, 23 accepted environment-dependent desktop skips;
strict clippy, release build, package staging and verification succeeded.
Baseline package SHA256: 85adf983ea13b819b02a4f14c245028ec160d6db823f55d8e489c99d1b25000b.

Installed acceptance passed for task, isolation, ulw, MCP, memory, LSP, auth,
doctor and three RPC-session drivers. Eval JS and Python passed; monitor failed.
This is not a final acceptance receipt.

Fresh monitor5 stderr proves the status subscriber calls LifecycleUi.theme with
a retired context, poisons ticker state and triggers a destructor abort.
After the status guard, monitor8 stderr shows a retired EventBus in
bind_monitor_events and a notification rejected by the retired runtime.
Monitor9 instrumentation records three separate notifier instances initially
active and an emission rejected as stale after the explicit reload request.
Temporary instrumentation has been removed.

Candidate fixes: serialize AgentSession.reload runtime replacement; suppress
retired status renders; abort and join the status subscriber before shutdown
finishes. The concurrent_reload_keeps_each_runtime_active_until_its_shutdown
regression subscribes to the first shutdown before starting the second reload,
polls the second future once and releases the first through Notify. It has no
fixed delay.

Monitor10 passed against binary SHA256
ccf2990d52e7dd328417aba930175649020e46c79b7d11711ee86228ad7d2906.
It observed senpi-monitor:notification after reload, killed bash_1 through the
registered tool, and proved the host, monitor and kernel exited with no owned
socket or temporary directory remaining. The probe used the release binary
directly; final staged-package acceptance is still required.
Core and terminal nextest passed 1,346 tests, zero skipped. Strict scoped clippy
is pending. No public push or deployment has occurred.

Diagnostic drivers close the RPC connection, terminate the owned host with a
bounded deadline, and retain process-exit proofs in their JSON artifacts.
No persistent diagnostic service was started. LSP diagnostics requests timed out;
compiler and clippy checks remain the available source validation.
