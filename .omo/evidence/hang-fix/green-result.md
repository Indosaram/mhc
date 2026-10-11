# Fixed regression result

Command: `CARGO_TARGET_DIR=$HOME/.cargo-target/maho-code/hg-integration CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo nextest run -p maho-core --test extension_lock_release` in `/home/projects/mhc-wt/hg-hang-fix`.

Captured from parent-owned terminal `bash_104`: exit code 0; summary `1 test run: 1 passed, 0 skipped`, test duration 0.184 seconds. Test: `pending_message_end_handler_does_not_block_renderer_snapshot`.

The bounded snapshot occurs while the first message_end handler is suspended. The handler is then released and prompt settlement is also asserted. Baseline proof is a separate run in `hg-hang-red`; it is not yet claimed here.
