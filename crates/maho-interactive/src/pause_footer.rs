//! Port of senpi packages/coding-agent/src/modes/interactive/pause-footer.ts.

pub const PAUSE_STATUS_KEY: &str = "session-pause";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PauseView {
    pub paused: bool,
    pub paused_notice_count: usize,
    pub run_paused: bool,
    /// Keys of the session's hold records (senpi `pauseView.records[].key`).
    pub record_keys: Vec<String>,
}

impl PauseView {
    /// The view senpi's `pauseFooterText(session)` reads off the live session.
    pub fn of_session(session: &maho_core::agent_session::AgentSession) -> Self {
        Self {
            paused: session.paused(),
            paused_notice_count: session.paused_notice_count(),
            run_paused: session.run_paused(),
            record_keys: session.holds().into_iter().map(|hold| hold.key).collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeCommandTarget {
    ResumeSession,
    SessionPicker,
}

pub fn pause_footer_text(view: &PauseView) -> Option<String> {
    if !view.paused && !view.run_paused {
        return None;
    }
    let held = if view.paused_notice_count == 1 {
        "1 update held".to_string()
    } else {
        format!("{} updates held", view.paused_notice_count)
    };
    if view.record_keys.iter().any(|key| key == "interrupted-run") {
        let held = if view.paused_notice_count > 0 {
            format!("{held} · ")
        } else {
            String::new()
        };
        return Some(format!(
            "Interrupted · {held}type to continue, /resume to resume"
        ));
    }
    if view.run_paused {
        Some(format!(
            "Run paused · {held} · type to continue, /resume to resume the run"
        ))
    } else {
        Some(format!(
            "Paused · {held} · type to continue, /resume to resume"
        ))
    }
}

pub fn resume_command_target(view: &PauseView) -> ResumeCommandTarget {
    if view.paused || view.run_paused {
        ResumeCommandTarget::ResumeSession
    } else {
        ResumeCommandTarget::SessionPicker
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_nothing_while_nothing_is_held() {
        let view = PauseView {
            paused: false,
            paused_notice_count: 0,
            run_paused: false,
            record_keys: Vec::new(),
        };
        assert_eq!(pause_footer_text(&view), None);
        assert_eq!(
            resume_command_target(&view),
            ResumeCommandTarget::SessionPicker
        );
    }

    #[test]
    fn names_the_held_updates_and_how_to_continue_after_a_stop() {
        let view = PauseView {
            paused: true,
            paused_notice_count: 1,
            run_paused: false,
            record_keys: Vec::new(),
        };
        assert_eq!(
            pause_footer_text(&view).as_deref(),
            Some("Paused · 1 update held · type to continue, /resume to resume")
        );
        assert_eq!(
            resume_command_target(&view),
            ResumeCommandTarget::ResumeSession
        );
    }

    #[test]
    fn says_the_run_is_paused_when_a_stop_paused_the_run_this_session_started() {
        let view = PauseView {
            paused: true,
            paused_notice_count: 3,
            run_paused: true,
            record_keys: Vec::new(),
        };
        assert_eq!(
            pause_footer_text(&view).as_deref(),
            Some("Run paused · 3 updates held · type to continue, /resume to resume the run")
        );
        assert_eq!(
            resume_command_target(&view),
            ResumeCommandTarget::ResumeSession
        );
    }

    #[test]
    fn resume_command_target_with_run_paused_only() {
        let view = PauseView {
            paused: false,
            paused_notice_count: 0,
            run_paused: true,
            record_keys: Vec::new(),
        };
        assert_eq!(
            resume_command_target(&view),
            ResumeCommandTarget::ResumeSession
        );
    }

    #[test]
    fn names_an_interrupted_run_and_omits_the_held_count_when_nothing_is_held() {
        let view = PauseView {
            paused: true,
            paused_notice_count: 0,
            run_paused: false,
            record_keys: vec!["interrupted-run".into()],
        };
        assert_eq!(
            pause_footer_text(&view).as_deref(),
            Some("Interrupted · type to continue, /resume to resume")
        );
        let held = PauseView {
            paused_notice_count: 2,
            ..view
        };
        assert_eq!(
            pause_footer_text(&held).as_deref(),
            Some("Interrupted · 2 updates held · type to continue, /resume to resume")
        );
    }
}
