# Concrete verification failures

maho-interactive nextest compilation exited 101 because runtime35_native.rs includes ignored evidence fixture .omo/evidence/task-35-slash/senpi-hotkeys-120.cells.json. The original generated fixture exists in the original project tree and was restored byte-for-byte through apply_patch, without generating or accepting new golden data. maho-interactive was appended to the current test queue for its justified rerun; the original failure log remains intact.

maho-ext-ask-user nextest exited 100 with three failures in native_question.rs: recovered_ui_failure_settles_orphaned_with_comment_and_tears_down (321), resumed_waiting_call_opens_original_request_without_duplicate_recovery (318), detached_timeout_queues_outcome_once_on_new_registered_runner (312). The first two report Asked-event context/request loss; the third reports Settled-event context/request/response loss. Read-only worker st_01a11914 is diagnosing exact production/fixture ownership. No tests have been skipped, removed, or weakened.

Context-config regression after preserving identical UI binding without invalidating runner session-manager adapters: all 50 ask-user tests passed. Config-resolution failed because its preservation fixture used deprecated categories.deep, correctly producing the new pinned alias diagnostic. The fixture now uses canonical deep-low and retains an empty-diagnostics assertion with diagnostic output.

Interactive supplemental runtime failure was another absent ignored fixture, not a cell mismatch: task-35-faux/senpi-hi-{40,80}.cells.json and senpi-hi.cells.json. All three original generated files were restored without auto-acceptance.

LSP faux_session failed because FauxSession::run_native now emits session_shutdown during disposal, while the pre-existing test expects only session_start and turn_end. This unrelated test expectation has not been edited to green. Its acceptance remains failed and must be reported or repaired under the actual lifecycle contract, not silently weakened.
