//! Port of senpi `packages/coding-agent/src/modes/interactive/unknown-command-feedback.ts`.
//!
//! senpi passes the rejection as `unknown` and checks `instanceof UnknownCommandError`; the Rust
//! caller passes the rejection's message when (and only when) it is an unknown-command rejection.

/// A `/...` submission typed after leading whitespace is sent as text instead of checked as a
/// command. `raw_text` is senpi's `EditorSubmitDetails.rawText` (the editor text before trimming).
pub fn submits_command_as_text(text: &str, raw_text: Option<&str>) -> bool {
    text.starts_with('/')
        && raw_text.is_some_and(|raw| raw.chars().next().is_some_and(char::is_whitespace))
}

/// senpi `UnknownCommandFeedbackTarget`.
pub trait UnknownCommandFeedbackTarget {
    fn editor_text(&self) -> String;
    fn set_editor_text(&mut self, text: &str);
    fn arm_confirmation(&mut self, text: &str);
    fn confirm_hint(&self) -> String;
    fn show_warning(&mut self, message: &str);
}

/// Turn an unknown-command rejection into editor feedback: the submitted text goes back into an
/// empty editor, the rejection is shown with the confirm hint, and submitting the same text again
/// sends it as a message. Returns `false` for any other error (`unknown_command_message` `None`).
pub fn report_unknown_command(
    unknown_command_message: Option<&str>,
    submitted_text: &str,
    target: &mut dyn UnknownCommandFeedbackTarget,
) -> bool {
    let Some(message) = unknown_command_message else {
        return false;
    };
    if target.editor_text().trim().is_empty() {
        target.set_editor_text(submitted_text);
    }
    target.arm_confirmation(submitted_text);
    let hint = target.confirm_hint();
    target.show_warning(&format!("{message}\n{hint}"));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Target {
        text: String,
        armed: Vec<String>,
        warnings: Vec<String>,
    }

    impl UnknownCommandFeedbackTarget for Target {
        fn editor_text(&self) -> String {
            self.text.clone()
        }
        fn set_editor_text(&mut self, text: &str) {
            self.text = text.to_string();
        }
        fn arm_confirmation(&mut self, text: &str) {
            self.armed.push(text.to_string());
        }
        fn confirm_hint(&self) -> String {
            "Press Enter again to send it as a message.".to_string()
        }
        fn show_warning(&mut self, message: &str) {
            self.warnings.push(message.to_string());
        }
    }

    #[test]
    fn a_leading_space_sends_slash_text_through_unchecked() {
        assert!(submits_command_as_text("/tmp/a.txt", Some(" /tmp/a.txt")));
        assert!(submits_command_as_text("/foo", Some("\t/foo")));
        assert!(!submits_command_as_text("/foo", Some("/foo")));
        assert!(!submits_command_as_text("/foo", None));
        assert!(!submits_command_as_text("foo", Some(" foo")));
    }

    #[test]
    fn restores_the_text_into_an_empty_editor_arms_confirmation_and_warns() {
        let mut target = Target::default();
        assert!(report_unknown_command(
            Some("Unknown command: /frob"),
            "/frob now",
            &mut target
        ));
        assert_eq!(target.text, "/frob now");
        assert_eq!(target.armed, ["/frob now"]);
        assert_eq!(
            target.warnings,
            ["Unknown command: /frob\nPress Enter again to send it as a message."]
        );
    }

    #[test]
    fn keeps_a_draft_typed_since_the_submission() {
        let mut target = Target {
            text: "new draft".to_string(),
            ..Target::default()
        };
        assert!(report_unknown_command(
            Some("Unknown command: /frob"),
            "/frob",
            &mut target
        ));
        assert_eq!(target.text, "new draft");
        assert_eq!(target.armed, ["/frob"]);
    }

    #[test]
    fn ignores_other_errors() {
        let mut target = Target::default();
        assert!(!report_unknown_command(None, "/frob", &mut target));
        assert!(target.armed.is_empty() && target.warnings.is_empty() && target.text.is_empty());
    }
}
