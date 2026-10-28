//! Port of senpi packages/coding-agent/src/modes/interactive/pause-footer.ts.

pub const PAUSE_STATUS_KEY: &str = "session-pause";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PauseView {
    pub paused: bool,
    pub paused_notice_count: usize,
    pub run_paused: bool,
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
        };
        assert_eq!(
            resume_command_target(&view),
            ResumeCommandTarget::ResumeSession
        );
    }
}
