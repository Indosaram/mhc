# Real TUI abort finding

Fully isolated 80x24 regular capture: startup idle, typed prompt, held stream, and steer echo all observed. Escape renders 'This operation was aborted' and restores steer-marker to the editor, but the previous Working line remains visible above the aborted message. The readiness predicate rejecting any visible Working line times out. Evidence: preinstall-surface-isolated/qa/tui-80x24-regular/{evidence.json,failure-screen.txt,held-frame.txt,startup.txt}.

This is not a blank surface or the earlier onboarding fixture issue. Native handle_event clears working_started_ms on AgentEnd (crates/maho-interactive/src/interactive_mode.rs:2431). Read-only diagnostic worker st_01a11913 is comparing render/event lifecycle and pinned source to determine a renderer defect versus fixture overreach. No assertion has been weakened, and the surface is not marked passing.
