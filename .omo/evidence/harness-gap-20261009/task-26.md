# Task 26 Evidence: Port comment checker release cache and runner (T16)

## Commands Run & Exit Codes

1. Baseline test and clippy runs:
   - Command: `$HOME/.cache/mhc-lanes/cargo-slot.sh cargo nextest run -p maho-comment-checker-core -p maho-omo-comment-checker`
   - Exit code: 0 (47 baseline tests passed)

2. Full test suite run after porting:
   - Command: `$HOME/.cache/mhc-lanes/cargo-slot.sh cargo nextest run -p maho-comment-checker-core -p maho-omo-comment-checker --no-capture`
   - Exit code: 0 (82 tests passed across 19 binaries, 0 skipped, 0 failed)

3. Clippy verification:
   - Command: `$HOME/.cache/mhc-lanes/cargo-slot.sh cargo clippy -p maho-comment-checker-core -p maho-omo-comment-checker --all-targets -- -D warnings`
   - Exit code: 0 (clean, 0 warnings)

## Git Metadata
- Base commit: `b3c3c94f2cf0220a7de0d3ac1f9434461fc14c02`
- Source tree hash: `32ade4991507d20105c926359836f6c516240f3d`

## QA Scenarios

### Happy Path QA
1. **Fake release download caches 0.8.0 marker once**:
   - Tested in `tests/downloader.rs` (`given_empty_cache_and_reachable_release_when_downloading_then_binary_lands_executable_and_archive_removed`):
     Downloads archive, verifies extraction, sets executable bit, creates version marker `comment-checker.version` containing `0.8.0\n`, removes temp archive, and only requests once.
   - Tested in `tests/stale_cache.rs` (`given_cached_checker_with_no_version_marker_when_downloading_then_pinned_replaces_and_records`):
     Stale cache without marker or with older 0.6.1 marker triggers fresh download of 0.8.0 release. Subsequent checks hit the cached release directly with 0 network requests.
2. **Explicit binary wins**:
   - Tested in `tests/resolver.rs` (`absolute_override_wins`):
     `OMO_COMMENT_CHECKER_BIN` environment variable pointing to an existing absolute binary takes precedence over package or path binaries.

### Failure QA
1. **Broken pipe / stdin failure**:
   - Tested in `tests/runner_stdin.rs` (`given_stdin_delivery_fails_when_checker_still_exits_2_then_child_is_killed_and_result_is_empty`):
     When stdin pipe breaks, SIGKILL is sent to the child process and empty result is returned.
   - Tested in `tests/stdin_pipe.rs` (`given_checker_binary_exiting_without_reading_stdin_when_checked_then_host_survives_and_clean`):
     Real process exiting 0 without reading stdin does not crash the host and returns a clean result without failure.
2. **Not started (EACCES / ENOEXEC / invalid command)**:
   - Tested in `tests/runner_exit_protocol.rs` (`given_the_spawn_fails_after_it_returned_eacces_when_run_then_the_start_failure_is_exposed`):
     Returns `CheckResult` with `failure: Some(CheckFailure { exit_code: None, stderr: "spawn /fake/comment-checker EACCES" })`.
   - Tested in `tests/start_failure.rs` (`given_checker_without_execute_permission_when_real_runner_spawns_it_then_start_failure_exposed`):
     Spawning a file with 0644 mode yields a `CheckFailure` with `exit_code: None` and stderr mentioning permission denied.
3. **Unexpected exit code**:
   - Tested in `tests/runner_exit_protocol.rs` (`given_the_checker_exits_outside_the_protocol_status_dll_not_found` and `generic_failure`):
     Exits 3221225781 and 1 yield `CheckFailure` with the respective exit code and stderr.
   - Tested in `tests/start_failure.rs` (`given_checker_exiting_outside_protocol_when_two_edits_finish_then_reported_once_and_inert`):
     Components mark themselves inert for the session, notifying the UI warning and logging once, without repeating on subsequent edits.

## Cleanup Receipt
- All temporary mock HTTP server sockets closed and bound ports released.
- All temporary directories from `tempfile::tempdir()` dropped and unlinked.
- No dangling background processes or child processes remain.
