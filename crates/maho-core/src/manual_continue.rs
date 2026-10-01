//! Port of senpi packages/coding-agent/src/core/manual-continue.ts.

pub const MANUAL_CONTINUE_CUSTOM_TYPE: &str = "manual-continue";

const MANUAL_CONTINUE_SHORTCUT: &str = ".";

/// Whether a submission is the manual-continue shortcut rather than user content. An empty session
/// has no intent to resume, and a "." carrying images is the user sending the images.
pub fn is_manual_continue_submission(text: &str, has_messages: bool, has_images: bool) -> bool {
    text.trim() == MANUAL_CONTINUE_SHORTCUT && has_messages && !has_images
}

pub const MANUAL_CONTINUE_DIRECTIVE: &str = "<system-notice>
Continue.

Resume the most recent intent and complete the unfinished work.
If you were interrupted mid-step, resume exactly where you stopped.
Never pause to summarize progress, re-confirm the plan, or ask whether to proceed; continue.
</system-notice>";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_bare_dot_with_messages_is_the_shortcut() {
        assert!(is_manual_continue_submission(".", true, false));
        assert!(is_manual_continue_submission("  .  ", true, false));
        assert!(!is_manual_continue_submission(".", false, false));
        assert!(!is_manual_continue_submission(".", true, true));
        assert!(!is_manual_continue_submission("hello", true, false));
        assert!(!is_manual_continue_submission("..", true, false));
    }

    #[test]
    fn the_custom_type_and_directive_match_senpi() {
        assert_eq!(MANUAL_CONTINUE_CUSTOM_TYPE, "manual-continue");
        assert!(MANUAL_CONTINUE_DIRECTIVE.starts_with("<system-notice>\nContinue."));
        assert!(MANUAL_CONTINUE_DIRECTIVE.ends_with("</system-notice>"));
    }
}
