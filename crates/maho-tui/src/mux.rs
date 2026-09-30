//! Port of senpi `packages/tui/src/mux.ts`.

use crate::process_env::{self, Env};

/// `isMultiplexerSession(env)`: tmux, GNU screen or zellij.
pub fn is_multiplexer_session_in(env: &Env) -> bool {
    ["TMUX", "TMUX_PANE", "STY", "ZELLIJ"]
        .iter()
        .any(|name| process_env::truthy(env, name))
}

/// `isMultiplexerSession()` against the current process environment.
pub fn is_multiplexer_session() -> bool {
    ["TMUX", "TMUX_PANE", "STY", "ZELLIJ"]
        .iter()
        .any(|name| process_env::var(name).is_some_and(|v| !v.is_empty()))
}

pub fn use_legacy_mux_render() -> bool {
    process_env::var("PI_TUI_LEGACY_MUX_RENDER").as_deref() == Some("1")
}

pub fn viewport_render_enabled() -> bool {
    process_env::var("PI_TUI_VIEWPORT_RENDER").as_deref() != Some("0")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_env::with_overrides;

    const CLEAR: [(&str, Option<&str>); 6] = [
        ("TMUX", None),
        ("TMUX_PANE", None),
        ("STY", None),
        ("ZELLIJ", None),
        ("PI_TUI_LEGACY_MUX_RENDER", None),
        ("PI_TUI_VIEWPORT_RENDER", None),
    ];

    fn cleared<R>(f: impl FnOnce() -> R) -> R {
        with_overrides(&CLEAR, f)
    }

    #[test]
    fn returns_true_when_each_multiplexer_environment_variable_is_set_alone() {
        for key in ["TMUX", "TMUX_PANE", "STY", "ZELLIJ"] {
            cleared(|| {
                with_overrides(&[(key, Some("x"))], || {
                    assert!(is_multiplexer_session(), "{key}")
                })
            });
        }
    }

    #[test]
    fn returns_false_when_multiplexer_environment_variables_are_unset() {
        cleared(|| assert!(!is_multiplexer_session()));
    }

    #[test]
    fn returns_false_for_empty_multiplexer_environment_strings() {
        cleared(|| {
            with_overrides(
                &[
                    ("TMUX", Some("")),
                    ("TMUX_PANE", Some("")),
                    ("STY", Some("")),
                    ("ZELLIJ", Some("")),
                ],
                || assert!(!is_multiplexer_session()),
            );
        });
    }

    #[test]
    fn reads_environment_variables_at_call_time_after_import() {
        cleared(|| {
            assert!(!is_multiplexer_session());
            with_overrides(&[("ZELLIJ", Some("0"))], || {
                assert!(is_multiplexer_session())
            });
            with_overrides(&[("ZELLIJ", Some(""))], || {
                assert!(!is_multiplexer_session())
            });
        });
    }

    #[test]
    fn returns_true_only_when_the_legacy_render_switch_is_exactly_1() {
        cleared(|| {
            with_overrides(&[("PI_TUI_LEGACY_MUX_RENDER", Some("1"))], || {
                assert!(use_legacy_mux_render())
            });
            for value in ["", "0", "true", "yes"] {
                with_overrides(&[("PI_TUI_LEGACY_MUX_RENDER", Some(value))], || {
                    assert!(!use_legacy_mux_render(), "{value}");
                });
            }
        });
    }

    #[test]
    fn defaults_to_enabled_when_the_viewport_render_switch_is_unset() {
        cleared(|| assert!(viewport_render_enabled()));
    }

    #[test]
    fn disables_only_when_the_viewport_render_switch_is_exactly_0() {
        cleared(|| {
            with_overrides(&[("PI_TUI_VIEWPORT_RENDER", Some("0"))], || {
                assert!(!viewport_render_enabled())
            });
            for value in ["1", "", "true", "yes"] {
                with_overrides(&[("PI_TUI_VIEWPORT_RENDER", Some(value))], || {
                    assert!(viewport_render_enabled(), "{value}");
                });
            }
        });
    }
}
