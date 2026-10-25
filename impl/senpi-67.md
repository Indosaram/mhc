# senpi-67 implementation report

Target: senpi 2026.10.23-1. Branch: p5147/senpi-67.

## Unit status

1. Session lifecycle ledger and facts: **INCOMPLETE**. Rust implementations: lifecycle_ledger.rs, lifecycle_record.rs, lifecycle_attention.rs, lifecycle_children.rs, lifecycle_delivery.rs, lifecycle_edit_leaf.rs, lifecycle_goal.rs, lifecycle_history.rs, lifecycle_human_input.rs, lifecycle_runtime_facts.rs; AgentSession::refresh_lifecycle now reads durable SessionWorkObligations and shares the typed session_lifecycle_reducer. Added adapter tests for terminal/hold precedence and pending admissions. Remaining concrete gaps: the existing session_lifecycle.rs controller is not yet backed by LifecycleLedger; complete turn-end effective-wake/failure evaluation is absent; custody changes are not yet wired to an independent live recompute subscription. Existing AgentSession integration therefore is not full upstream controller parity.

2. Extension session-control and move lifecycle API: **INCOMPLETE**. Existing typed SessionRelocationHost is reused by ExtensionActions; duplicate relocation fields/methods and duplicate itemized extension signatures removed. AgentSession::emit_session_move and extension runner provide ordered move export/import dispatch; extensions/session_move_emit.rs includes ordered failure collection tests. Existing provider request preparation remains in SessionExtensionContext::prepare_provider_request. Tool-barrier extension methods now bind to the existing session barrier. Remaining concrete gaps: move hooks are not automatically orchestrated by session release/import; tool workspace-effect registration and cancellation propagation still require review. Tool dispatch now waits on the session barrier and agent-end clears it; the barrier wait registers its notification while holding the armed-state lock to avoid losing a concurrent release.

3. Held inputs and itemized delivery: **INCOMPLETE**. held_inputs.rs implements durable custody; external_admission.rs tracks input holds and settled callbacks; prompt registers admission work before awaits. Itemized held sends now use the pre-existing typed HeldItem path, preserving one notice implementation and the actual persisted entry ID. Remaining concrete gap: automatic admission of held person inputs into their releasing turn (including command ordering, manual continue, and settlement-only consumption) is not implemented. Current deliver_held_inputs prompts individually and must not be called equivalent to upstream _takeHeldInputs.

4. Lost-lock write fencing: **DONE**. file_lock.rs implements loss handling; lockfile_policy.rs uses HeldFileLock while retaining integration broker support. Credential state and move ledger write paths assert lock ownership. Rebase retained the shared ownership/custody implementation rather than replacing it.

5. Tool interruption result: **DONE**. interrupted_tool_result.rs and AgentSession stop_interrupt integration mark interrupted failures while preserving details and avoiding implicit retry.

6. Footer subagent cost: **DONE**. footer_data_provider.rs validates/resets subagent-tree cost; interactive footer rendering includes it.

## Verification

No cargo build/check/test/clippy run (global-gate restriction). Focused Rust tests were added but not executed. rustfmt --edition 2024 --config skip_children=true --check passed for agent_session.rs, engine_turn_hold.rs, engine_turn_hold_notices.rs, lifecycle_runtime_facts.rs and maho-ext-api/src/types.rs before the final report commit; git diff --check passed. merge-tree --write-tree parity/omo-5.1.47 p5147/senpi-67 exited 0, tree a113bc15fc0d66368bcd7df36062aec309c3c816 (before report-only commit).

This branch is partial work, not full completion of the requested three remaining units. The gaps above are missing concrete runtime wiring, not a claim of completed parity or a verified build.
