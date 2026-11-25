//! Port stub of `job-widget-tap.ts`:
//! Upstream offers each background session to `job-widgets`. `job-widgets` is a low-priority
//! group outside this port, so this module provides a clean no-op seam preserving the
//! exact call boundaries in `extension_state.rs`.

use crate::runtime_session::TerminalRuntimeSession;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobWidgetBrief {
    pub command: String,
    pub description: String,
}

pub fn tap_background_session(
    _session_id: Option<&str>,
    _id: &str,
    _runtime: &TerminalRuntimeSession,
    _brief: JobWidgetBrief,
) {
}

pub fn untap_background_session(_runtime: Option<&TerminalRuntimeSession>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untap_without_runtime_is_a_noop() {
        untap_background_session(None);
    }
}
