//! Detect whether we run inside cmux (cmux omo).
//!
//! When cmux-omo sets up the environment it injects a tmux shim and sets
//! `CMUX_SOCKET_PATH` / `TMUX`. If detected, tmux commands are redirected to
//! `cmux __tmux-compat` so they become native cmux splits.

use crate::env_source::{EnvSource, ProcessEnv, is_set};

pub fn is_cmux_compat_environment(environment: &dyn EnvSource) -> bool {
    let tmux = environment.var("TMUX");
    let tmux = tmux.as_deref();
    tmux.is_some_and(|value| value.contains("cmuxterm"))
        || (is_set(environment.var("CMUX_SOCKET_PATH").as_deref()) && !is_set(tmux))
}

/// [`is_cmux_compat_environment`] against the live process environment.
#[must_use]
pub fn is_cmux_compat() -> bool {
    is_cmux_compat_environment(&ProcessEnv)
}
