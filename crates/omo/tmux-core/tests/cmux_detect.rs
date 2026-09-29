//! Port of src/cmux-detect.test.ts

mod common;

use common::env;
use tmux_core::is_cmux_compat_environment;

#[test]
fn tmux_contains_cmuxterm_returns_true() {
    let environment = env(&[("TMUX", "/tmp/cmuxterm-12345.sock,1234,0")]);
    assert!(is_cmux_compat_environment(&*environment));
}

#[test]
fn standard_tmux_without_cmuxterm_returns_false() {
    let environment = env(&[("TMUX", "/tmp/tmux-1000/default,1234,0")]);
    assert!(!is_cmux_compat_environment(&*environment));
}

#[test]
fn cmux_socket_path_without_tmux_returns_true() {
    let environment = env(&[("CMUX_SOCKET_PATH", "/var/run/cmux.sock")]);
    assert!(is_cmux_compat_environment(&*environment));
}
