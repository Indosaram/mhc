# tmux-core notes

## Verification
- `just test -p tmux-core`: 96 tests run, 96 passed, 0 skipped.
- `cargo clippy -p tmux-core --all-targets -- -D warnings`: clean.
- `cargo fmt -p tmux-core --check`: clean.

## Red/green proof
The mutation was in `src/tmux_utils/pane_close.rs`, where the graceful-close check `contains("can't find pane")` was changed to `contains("MUTATED can't find pane")`.

- Red: `cargo test -p tmux-core --test pane_close` gave 4 passed and 2 failed. The failures were `pane_already_closed_by_ctrl_c_returns_true` and `cmux_eager_pane_exited_naturally_is_graceful_success`.
- Green: with the source restored, the same command gave 6 passed and 0 failed.
