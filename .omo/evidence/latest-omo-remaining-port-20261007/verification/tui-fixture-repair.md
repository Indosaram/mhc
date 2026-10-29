# TUI fixture repair

The first fresh tui run timed out at startup. Its captured screen shows the editor and offline model footer but also a working turn before the test prompt. The old harness marked cwd/.maho/onboarding-completed; the current mounted composition resolves agent-global state at <agentDir>/omo-senpi/omo-native (crates/maho-cli/src/cli/omo_mount.rs:43-49). Onboarding therefore triggered a follow-up turn held by the gated provider.

The offline-agent fixture now writes onboarding-completed into that exact agent-global state directory when it creates the isolated agent home. Product source and readiness assertions are unchanged. Failed artifacts remain in preinstall-surface; the corrected run uses preinstall-surface-repaired. The obsolete monitor mon_CMCRPSWZ17545DE4 was killed with its process tree; its aggregate cleanup receipt was not completed and must not be claimed successful.

A second fixture boundary was found: the session exports OMO_CODING_AGENT_DIR and SENPI_CODING_AGENT_DIR, and product identity prioritizes these over HOME. The PTY driver now explicitly binds both variables to its isolated agent root rather than inheriting the session root. No auth/config contents were read or printed. The current mixed run cannot serve as final all-geometry evidence; installed scenario-all will use the corrected fixture for every case.
