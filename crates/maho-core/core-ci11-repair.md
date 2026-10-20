# CI11 core repair

## Scope and execution

- Worktree: /home/projects/mhc-wt/nonwindows-20261020.
- Sole ownership: crates/maho-core/**. Task st_01a159d7 is session 01a159d2-7d27-7a4f-a71b-143d61c76972; there are no delegated children.
- Read all thirteen full logs under /home/projects/mhc/.omo/nonwindows-20261020/ci-39440147124/ using bounded read calls, including Linux and Windows failure blocks.
- Only apply_patch edits. No cargo, tests, CI, diagnostics commands, or commits run. The patch tool emitted an automatic LSP diagnostic timeout (not a Rust error); no diagnostic retry was requested.
- These are source-authored repairs, not a passing execution claim. Deferred stack-overflow correction remains unverified until the parent's final gate.

## Exact upstream evidence

The worktree tools/golden/pin.mjs pins 903b83367545bea516f35fc7f7c7cb692847988d. Read that checkout at /tmp/parity-up/senpi-1019 (git worktree list identifies HEAD 903b83367), not the unrelated /home/indo/code/senpi HEAD fe8c564bf.

- packages/agent/src/agent-loop.ts:581-595 builds provider context with normalizeContext. packages/ai/src/utils/transcript.ts:31-36 folds systemPrompt/tools into the initial system message and returns only messages/activeToolNames. A custom stream must inspect transcript state rather than legacy shorthand fields.
- packages/ai/src/providers/anthropic.ts:90-105 declares apiKey only, explicitly excluding Claude subscription OAuth. Subscription OAuth belongs to the anthropic-subscription extension. packages/coding-agent/src/core/auth-storage.ts:858-869 enumerates declared builtin OAuth plus dynamic providers.
- packages/coding-agent/src/core/model-runtime.ts:663-679 intentionally bypasses the availability snapshot for scoped getAvailable(providerId). getProviderAuthStatus/resolveProviderAuth at 867 onward reads snapshot.auth after stored/configured precedence.

## Core dispositions

| CI symptom | Disposition | Patch |
| --- | --- | --- |
| active_tool_change_rebuilds_prompt_in_next_provider_request, Linux test__4_.log:3241 onward; observed [None] instead of [Some(rebuilt)] | Pin-proven stale observation; no runtime prompt loss | Preserve rebuilt-vs-old and exact request equality assertions; capture get_current_system_prompt(&context.messages). No agent_loop change. |
| deferred_post_compaction_inputs_deliver_once_after_first_response, Linux test__4_.log:3321 onward; SIGABRT stack overflow | Runtime future frame pressure; source repair, execution unverified | Box queue_user_input at steer/follow_up boundaries and both extension dispatch futures within run_input_handlers. Existing boxed emit convention already documents large extension event state machines. No queue ordering, cancellation, persistence, timeout, three-case coverage, or test stack limits changed. |
| builtin_oauth_providers_come_from_provider_auth_declarations, Linux test__4_.log:3374 onward | Pin-proven stale Anthropic OAuth expectation | Assert Anthropic is absent; retain radius and Devin declarations. |
| builtin_providers_still_enumerate_after_dynamic_registration, Linux test__4_.log:3332 onward | Pin-proven stale builtin chosen | Require radius and Devin alongside dynamically registered anthropic-subscription. |
| ambient_auth_admits_models_without_synthetic_api_keys, Linux test__4_.log:3416 onward | Pin-proven stale snapshot sequence | Retain scoped availability len=1 and no synthetic key assertions; request full get_available(None) before checking synchronous configured/ambient snapshot status. Production scoped behavior unchanged. |
| clone_on_copy at agent_session.rs:9348/9365, clippy.log:1954-1972 and windows.log:1935-1953 | Mechanical lint repair | Use Copy cause directly; preserve all typed-error-cause persistence assertions. |

Windows core runtime tests did not execute: windows-test__3_.log failed compilation of maho-omo-side-panel first. Windows Copy lint failures duplicate Linux sites. No inference of a Windows runtime pass is made.

## All thirteen job dispositions

- build.log: no failure block.
- test__1_.log and test__2_.log: no failure blocks.
- clippy.log and windows.log: core two Copy lints repaired; external maho-ext-compaction repair_tool_pairs.rs:102 cloned_ref_to_slice_refs left to owner.
- macos.log: same external maho-ext-compaction lint; no core runtime results.
- test__3_.log and windows-test__3_.log: external maho-omo-side-panel NativeBody.rendered E0560; core Windows runtime blocked by compile failure.
- test__5_.log: external maho-ext-host test runner Model missing prompt_cache/supports_assistant_prefill E0063.
- test__6_.log: external maho-cli mini_server five E0308 mismatches.
- windows-test__2_.log: external maho-cli credentials five ApiKeyCredential.extra E0063 failures.
- test__4_.log: 46 failures, five core runtime failures covered above; remaining codemode and compaction owned externally.
- windows-test__1_.log: 47 external runtime failures; enumerated individually below. No core runtime failure blocks in this job.

## Every runtime failure block

Repeated nextest summary rows are deduplicated, not omitted failures. Each distinct failing test in each platform log has a disposition.

| Log | Failing test | Disposition |
| --- | --- | --- |
| test__4_.log | maho-codemode::active_kernel_prelude test_active_tool_kernel_prelude_js_execution | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::completion_handle_session real_eval_completion_handle_survives_cells_but_not_session_switch | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::eval_model_context callable_eval_uses_each_invocations_model_for_display_images | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::extension_factory completion_context_refreshes_for_same_model_id_without_reinstalling_eval | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::extension_factory concurrent_completion_keeps_each_invocations_context | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::extension_factory registered_callable_enforces_configured_detached_capacity | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::host_package_installs host_slot_is_fifo_and_interrupt_retains_worker_state | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::host_package_installs local_packages_accumulate_and_resolve_in_the_persistent_kernel | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_kernel cooperative_interrupt_preserves_live_worker_globals | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_kernel bridge_wait_timeout_retains_worker_state | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_kernel cooperative_interrupt_settles_nested_host_wait_as_stale | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_kernel forced_retirement_retires_tracked_child_outside_worker_group | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_kernel forced_worker_retirement_settles_nested_host_wait_as_stale | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_kernel live_kernel_tool_host_pump_describes_and_invokes_worker_definition | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_kernel pulled_host_call_reply_resumes_real_worker | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_kernel live_name_sources_refresh_before_every_cell_and_recovery | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_kernel queued_cancel_never_executes_or_starts_and_preserves_parent | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_kernel real_bun_persistent_kernel | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_kernel reset_fences_old_kernel_tool_generation | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_kernel scoped_kernel_tool_denies_host_call_without_leaking_scope | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::js_run_queue removed_cell_settles_while_neighbors_preserve_order | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::kernel_tools_scope adapter_cancels_pending_worker_call_and_preserves_failure_message | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::kernel_tools_scope real_eval_consumer_retains_live_capability_and_nested_scope_restores_outer | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::local_module_loader escaped_static_sources_execute_in_actual_worker | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::local_module_loader prepared_imports_execute_in_external_worker | External owner; parent coordination; no edits |
| test__4_.log | maho-codemode::process_isolation direct_process_preserves_state_and_replaces_crashed_child | External owner; parent coordination; no edits |
| test__4_.log | maho-core agent_session::tests::active_tool_change_rebuilds_prompt_in_next_provider_request | Core disposition below |
| test__4_.log | maho-core agent_session::tests::deferred_post_compaction_inputs_deliver_once_after_first_response | Core disposition below |
| test__4_.log | maho-core auth_storage::tests::builtin_providers_still_enumerate_after_dynamic_registration | Core disposition below |
| test__4_.log | maho-core auth_storage::tests::builtin_oauth_providers_come_from_provider_auth_declarations | Core disposition below |
| test__4_.log | maho-core model_runtime::tests::ambient_auth_admits_models_without_synthetic_api_keys | Core disposition below |
| test__4_.log | maho-ext-compaction::message_safety faux_provider_accepts_repaired_messages | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration native_empty_summary_uses_required_deterministic_fallback | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration native_overflow_shrinks_summary_request_before_continuation | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration native_real_session_consumes_all_nondefault_fractional_settings | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration native_overthreshold_prompt_compacts_before_provider_turn | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration native_registered_cancellation_is_subscribed_and_never_persists | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration native_registered_compaction_matches_pinned_source_lifecycle | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration native_repeated_requests_do_not_repeat_reminder_lease | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration native_session_compaction_uses_registered_generator_and_persists_metadata | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration registered_idle_retry_dispatches_without_prompt_arrival | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration registered_prompt_arrival_stands_down_pending_idle_retry | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration registered_remote_fallback_payloads_match_source_branches | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration native_registered_idle_retired_runner_rejects_late_apply | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration native_registered_idle_summary_is_superseded_by_model_change | External owner; parent coordination; no edits |
| test__4_.log | maho-ext-compaction::registration native_registered_idle_summary_is_superseded_by_shutdown | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::active_kernel_prelude test_active_tool_kernel_prelude_js_execution | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::completion_handle_session real_eval_completion_handle_survives_cells_but_not_session_switch | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::extension_factory completion_context_refreshes_for_same_model_id_without_reinstalling_eval | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::extension_factory registered_callable_enforces_configured_detached_capacity | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::host_package_installs host_slot_is_fifo_and_interrupt_retains_worker_state | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::host_package_installs local_packages_accumulate_and_resolve_in_the_persistent_kernel | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::js_kernel bridge_wait_timeout_retains_worker_state | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::js_kernel cooperative_interrupt_settles_nested_host_wait_as_stale | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::js_kernel cooperative_interrupt_preserves_live_worker_globals | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::js_kernel forced_worker_retirement_settles_nested_host_wait_as_stale | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::js_kernel live_kernel_tool_host_pump_describes_and_invokes_worker_definition | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::js_kernel pulled_host_call_reply_resumes_real_worker | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::js_kernel queued_cancel_never_executes_or_starts_and_preserves_parent | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::js_kernel reset_fences_old_kernel_tool_generation | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::js_kernel real_bun_persistent_kernel | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::js_kernel scoped_kernel_tool_denies_host_call_without_leaking_scope | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::js_run_queue removed_cell_settles_while_neighbors_preserve_order | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::kernel_tools_scope adapter_cancels_pending_worker_call_and_preserves_failure_message | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::kernel_tools_scope real_eval_consumer_retains_live_capability_and_nested_scope_restores_outer | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::local_module_loader escaped_static_sources_execute_in_actual_worker | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::local_module_loader prepared_imports_execute_in_external_worker | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::process_isolation direct_process_preserves_state_and_replaces_crashed_child | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-codemode::session_cwd async_and_sync_checks_classify_unavailable_paths | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-memory-core sync::crypto::batch_tests::corrupted_objects_return_no_records_and_one_reason | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-memory-core sync::crypto::unwrap_tests::golden_history_requires_exact_origins_and_binding | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-memory-core sync::crypto::trust_pin_tests::pin_is_write_once_and_corruption_never_resets_trust | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-memory-core sync::item::render_tests::concurrent_kind_matrix_converges_and_recaptures_as_a_fixed_point | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-memory-core sync::item::tests::disk_patch_handles_multibyte_keys_block_values_removal_and_addition | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-memory-core sync::keyring_port::tests::preserves_canonical_refusal_taxonomy_for_records_and_snapshots | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-memory-core sync::recovery::recovery_tests::acknowledged_but_missing_join_and_rotation_never_release_writes | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-memory-core sync::recovery::recovery_tests::enrolled_key_and_last_key_confirmation_use_real_verified_membership | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-memory-core sync::recovery::recovery_tests::removal_pending_resumes_rotation_without_a_second_removal | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-memory-core sync::recovery::recovery_tests::verified_rotation_releases_the_hold | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-memory-core sync::recovery::recovery_tests::withheld_rotation_leaves_a_successfully_joined_device_persistently_read_only | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-senpi-desktop-tool::cua_adapter blast_radius_in_batch_screenshot_reanchors_and_first_failure_stops_everything | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-senpi-desktop-tool::cua_adapter display_stale_pointer_input_without_current_frame_is_computer_display_stale | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-senpi-desktop-tool::cua_adapter kill_switch_bypass_stop_path_refusal_halts_batch_and_never_dispatches_followup | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-senpi-desktop-tool::cua_adapter permission_revoked_os_refusal_maps_to_computer_permission_required | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-senpi-desktop-tool::cua_adapter out_of_bounds_drift_max_minus_one_lands_max_and_neg_refused | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-senpi-desktop-tool::cua_adapter runaway_loop_halt_batch_deadline_cancels_step_that_never_finishes | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-senpi-desktop-tool::cua_adapter suspended_enforcement_suspended_session_refuses_input_and_stops_batch | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-senpi-desktop-tool::execute reaches_host_tools_from_run_code_through_injected_execute_tool | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-senpi-desktop-tool::execute refuses_input_inside_read_only_run_before_it_reaches_engine | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-senpi-desktop-tool::execute runs_window_screenshot_chain_through_facade_and_returns_inline_image | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-senpi-desktop-tool::permission_guidance marks_denial_inside_details_so_code_mode_marshal_reports_has_error | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-senpi-desktop-tool::permission_guidance carries_engine_permission_metadata_through_public_computer_result | External owner; parent coordination; no edits |
| windows-test__1_.log | maho-memory-core sync::item::property_tests::three_device_seeded_capture_model_converges_without_spliced_values | External owner; parent coordination; no edits |

## Parent coordination and unresolved execution evidence

Initial suspicion that maho-agent build_provider_context discarded the native stream contract was explicitly withdrawn after reading exact pinned normalizeContext. Parent was sent the correction and identity mapping through thread_send; task_send cannot address the parent session here. No maho-agent grant or edit is needed.

All non-core failures above remain with their assigned owners. No root/Cargo/CI change is requested by this lane. Parent must run its authorized final merged checks; particularly confirm the deferred test no longer overflows, plus all retained prompt/auth/persistence assertions. The stack-pressure diagnosis is based on source frame composition and the logged stack abort, not a captured backtrace (stack abort produced none), so it is not represented as execution-confirmed.

