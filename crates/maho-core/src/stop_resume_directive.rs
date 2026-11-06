//! Resume interruption directive: hidden system reminder when a stopped turn resumes.
//!
//! Port of senpi `packages/coding-agent/src/core/stop-resume-directive.ts`.

pub const STOP_RESUME_CUSTOM_TYPE: &str = "session-resumed";

pub const STOP_RESUME_DIRECTIVE_TEXT: &str = "<system-reminder>The user stopped the previous turn and has now resumed. Tool calls that the Stop interrupted show \
    an aborted result; they were not re-run and their side effects may be partial. Check their effects before repeating \
    any of them, then continue the work.</system-reminder>";

pub const INTERRUPTED_RESUME_DIRECTIVE_TEXT: &str = "<system-reminder>The previous run was interrupted and has now resumed. Interrupted tool calls may have \
    partial side effects and were not re-run. Check their effects before repeating any of them, then continue \
    the work.</system-reminder>";

/// Why a held run resumes: a user Stop, or an interrupted run reopened (senpi#4204).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResumeCause {
    #[default]
    Stop,
    Interrupted,
}

impl ResumeCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopResumeDirective {
    pub custom_type: String,
    pub content: String,
    pub display: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

pub fn stop_resume_directive(cause: ResumeCause) -> StopResumeDirective {
    match cause {
        ResumeCause::Interrupted => StopResumeDirective {
            custom_type: STOP_RESUME_CUSTOM_TYPE.to_owned(),
            content: INTERRUPTED_RESUME_DIRECTIVE_TEXT.to_owned(),
            display: false,
            details: Some(serde_json::json!({ "resumeCause": cause.as_str() })),
        },
        ResumeCause::Stop => StopResumeDirective {
            custom_type: STOP_RESUME_CUSTOM_TYPE.to_owned(),
            content: STOP_RESUME_DIRECTIVE_TEXT.to_owned(),
            display: false,
            details: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_properties() {
        let directive = stop_resume_directive(ResumeCause::Stop);
        assert_eq!(directive.custom_type, "session-resumed");
        assert!(!directive.display);
        assert!(
            directive
                .content
                .contains("The user stopped the previous turn")
        );
        assert!(directive.details.is_none());
    }

    /// senpi test/suite/regressions/4204-interrupted-resume.test.ts: the interrupted cause names the
    /// interruption and marks `details.resumeCause`.
    #[test]
    fn interrupted_directive_marks_its_cause() {
        let directive = stop_resume_directive(ResumeCause::Interrupted);
        assert_eq!(directive.custom_type, STOP_RESUME_CUSTOM_TYPE);
        assert!(!directive.display);
        assert!(
            directive
                .content
                .contains("The previous run was interrupted and has now resumed")
        );
        assert_eq!(
            directive.details,
            Some(serde_json::json!({"resumeCause": "interrupted"}))
        );
    }
}
