# CI11 codemode repair batch

Status: source repairs complete; runtime verification deliberately not run. All 27 unique codemode failures have a disposition below. No cargo, tests, CI, commits, timeout increases, or skips were performed by this batch. No files outside codemode were edited except this requested report. Prior CI8-CI10 repairs were retained.

## Evidence and causes

Evidence: `/home/projects/mhc/.omo/nonwindows-20261020/ci-39440147124/test__4_.log`, `windows-test__1_.log`, and `/home/projects/mhc/.omo/nonwindows-20261020/ci11-ledger.md`. The union is 26 Linux failures and 23 Windows failures, 27 unique codemode tests.

R1: `src/kernels/js/worker-package-resolve.js` contained eight literal standalone ellipses and a persisted `[Elided source: ...]` footer. These were actual source bytes, not display truncation. Restored the COMPLETE implementation from the installed upstream source at `/home/indo/.omo/agent/runtime/599a2a2dfad737d9-479c91a6c9b4/node_modules/@code-yeongyu/senpi-codemode/src/kernels/js/worker-package-resolve.js`. Restoration includes filesystem probes, conditional exports and patterns, legacy entries, node_modules traversal, realpaths, builtins, and ordered roots. No functions were stubbed. All 31 JavaScript files under the crate were inventoried for standalone ellipses and persisted elision footers; only this resolver matched.

`LocalModuleLoader::new` embeds this source in a data-URL import and installs its promise in every prepared cell's prelude. Consequently the malformed module affects ordinary cells as well as imports. Bare imports await that rejected promise; ordinary cells can race its unhandled rejection and lose the worker. This accounts for both explicit `Unexpected …` errors and secondary missing values, stale descriptors, closed channels, worker replacement, and incomplete invocation lists. These are root-cause dispositions, not claims that the repaired tests have passed.

R2: `tests/js_run_queue.rs` expected `stopped_result` for a queued cancellation even though `JavaScriptRunQueue::remove` intentionally emits `unstarted_result`, including `kernelState: "not-run"`. Updated the exact assertion to the intended unstarted result. The production cancellation metadata and existing tests that require it remain intact.

R3: Windows metadata reports `NotFound` for a child beneath a regular file; Unix reports `NotADirectory`. The cwd validator intentionally preserves the native classification as `ENOENT` or `ENOTDIR`. Updated `tests/session_cwd.rs` to require the exact platform result for that case. Missing paths still require `NotFound`/`ENOENT`; a file used directly as cwd still requires `NotADirectory`/`not a directory`. Both async and sync paths remain asserted. No production cwd behavior changed.

## Every codemode failure

All names below have the prefix `maho-codemode::`. L = Linux test4; W = Windows test1. R1 means restored complete resolver; R2 and R3 are the exact assertion repairs described above. Each awaits the parent's final merged verification run.

| # | Test | CI11 | Disposition |
|---|---|---|---|
| 1 | active_kernel_prelude::test_active_tool_kernel_prelude_js_execution | L,W | R1; prelude cell must survive and invoke the active tool. |
| 2 | completion_handle_session::real_eval_completion_handle_survives_cells_but_not_session_switch | L,W | R1; preserve successful completion output rather than parsing error text. |
| 3 | eval_model_context::callable_eval_uses_each_invocations_model_for_display_images | L | R1; display cell failed with Unexpected ellipsis before image delivery. |
| 4 | extension_factory::completion_context_refreshes_for_same_model_id_without_reinstalling_eval | L,W | R1; completion executed but malformed module rejection poisoned settlement. |
| 5 | extension_factory::concurrent_completion_keeps_each_invocations_context | L | R1; worker loss prevented the second invocation from reaching completion. |
| 6 | extension_factory::registered_callable_enforces_configured_detached_capacity | L,W | R1; early cell error replaced expected detached state. |
| 7 | host_package_installs::host_slot_is_fifo_and_interrupt_retains_worker_state | L,W | R1; missing persistent value was downstream of worker failure. Host cancellation logic retained. |
| 8 | host_package_installs::local_packages_accumulate_and_resolve_in_the_persistent_kernel | L,W | R1; imports directly awaited malformed resolver module. |
| 9 | js_kernel::cooperative_interrupt_preserves_live_worker_globals | L,W | R1; rejected module caused forced worker loss, defeating cooperative retention. |
| 10 | js_kernel::bridge_wait_timeout_retains_worker_state | L,W | R1; module failed after 7-11 ms instead of reaching existing 400 ms timeout. Timeout unchanged. |
| 11 | js_kernel::cooperative_interrupt_settles_nested_host_wait_as_stale | L,W | R1; definition lost its worker before nested invocation. |
| 12 | js_kernel::forced_retirement_retires_tracked_child_outside_worker_group | L | R1; setup worker died before child observation. Retirement behavior unchanged. |
| 13 | js_kernel::forced_worker_retirement_settles_nested_host_wait_as_stale | L,W | R1; setup descriptor was stale after worker loss. |
| 14 | js_kernel::live_kernel_tool_host_pump_describes_and_invokes_worker_definition | L,W | R1; description failed with JavaScript worker reset. |
| 15 | js_kernel::pulled_host_call_reply_resumes_real_worker | L,W | R1; tool reply path reached but final value was missing after module failure. |
| 16 | js_kernel::live_name_sources_refresh_before_every_cell_and_recovery | L | R1; module syntax failure masked the expected collision error. |
| 17 | js_kernel::queued_cancel_never_executes_or_starts_and_preserves_parent | L,W | R1; parent value missing after worker failure. Queue exclusion preserved. |
| 18 | js_kernel::real_bun_persistent_kernel | L,W | R1; Linux reported Unexpected ellipsis; Windows lost x after worker replacement and reported ReferenceError. |
| 19 | js_kernel::reset_fences_old_kernel_tool_generation | L,W | R1; setup tool descriptor lost before intentional reset. |
| 20 | js_kernel::scoped_kernel_tool_denies_host_call_without_leaking_scope | L,W | R1; setup tool descriptor lost before scope proof. |
| 21 | js_run_queue::removed_cell_settles_while_neighbors_preserve_order | L,W | R2; exact expected cancellation frame includes not-run state. |
| 22 | kernel_tools_scope::adapter_cancels_pending_worker_call_and_preserves_failure_message | L,W | R1; Linux lacked descriptor generation; Windows call closed after worker loss. |
| 23 | kernel_tools_scope::real_eval_consumer_retains_live_capability_and_nested_scope_restores_outer | L,W | R1; real consumer returned Unexpected ellipsis instead of complete state. |
| 24 | local_module_loader::escaped_static_sources_execute_in_actual_worker | L,W | R1; import failed building corrupted data-URL resolver. |
| 25 | local_module_loader::prepared_imports_execute_in_external_worker | L,W | R1; import failed building corrupted data-URL resolver. |
| 26 | process_isolation::direct_process_preserves_state_and_replaces_crashed_child | L,W | R1; initial identity missing after resolver rejection. Process protocol unchanged. |
| 27 | session_cwd::async_and_sync_checks_classify_unavailable_paths | W | R3; exact platform filesystem classification retained. |

## Desktop collateral, no desktop edits

The desktop owner independently confirmed these 12 Windows failures share R1. All names have the prefix `maho-senpi-desktop-tool::`. Their tests and product files remain unchanged; the codemode asset restoration is their disposition, subject to the parent's verification.

1. cua_adapter::blast_radius_in_batch_screenshot_reanchors_and_first_failure_stops_everything
2. cua_adapter::display_stale_pointer_input_without_current_frame_is_computer_display_stale
3. cua_adapter::kill_switch_bypass_stop_path_refusal_halts_batch_and_never_dispatches_followup
4. cua_adapter::permission_revoked_os_refusal_maps_to_computer_permission_required
5. cua_adapter::out_of_bounds_drift_max_minus_one_lands_max_and_neg_refused
6. cua_adapter::runaway_loop_halt_batch_deadline_cancels_step_that_never_finishes
7. cua_adapter::suspended_enforcement_suspended_session_refuses_input_and_stops_batch
8. execute::reaches_host_tools_from_run_code_through_injected_execute_tool
9. execute::refuses_input_inside_read_only_run_before_it_reaches_engine
10. execute::runs_window_screenshot_chain_through_facade_and_returns_inline_image
11. permission_guidance::marks_denial_inside_details_so_code_mode_marshal_reports_has_error
12. permission_guidance::carries_engine_permission_metadata_through_public_computer_result

## Handoff

Changed files: resolver source, `tests/js_run_queue.rs`, `tests/session_cwd.rs`, and this report. Cancellation completion, host abort, queue liveness, worker retirement, generation fencing, process protocol, and every timeout/skip setting were left intact. Other owners' core, builtins, memory, CLI, and desktop files were not edited.

No new provider abort occurred during the resumed repair. The session_cwd patch tool automatically attempted diagnostics and returned a diagnostic-fetch timeout (3000 ms), not a Rust diagnostic; no explicit LSP, syntax check, test, cargo, or CI command was run. Parent verification is required before declaring any failure green.
