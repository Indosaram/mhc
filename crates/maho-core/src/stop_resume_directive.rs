//! Resume interruption directive: hidden system reminder when a stopped turn resumes.
//!
//! Port of senpi `packages/coding-agent/src/core/stop-resume-directive.ts`.

pub const STOP_RESUME_CUSTOM_TYPE: &str = "session-resumed";

pub const STOP_RESUME_DIRECTIVE_TEXT: &str =
    "<system-reminder>The user stopped the previous turn and has now resumed. Tool calls that the Stop interrupted show \
    an aborted result; they were not re-run and their side effects may be partial. Check their effects before repeating \
    any of them, then continue the work.</system-reminder>";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopResumeDirective {
    pub custom_type: String,
    pub content: String,
    pub display: bool,
}

pub fn stop_resume_directive() -> StopResumeDirective {
    StopResumeDirective {
        custom_type: STOP_RESUME_CUSTOM_TYPE.to_owned(),
        content: STOP_RESUME_DIRECTIVE_TEXT.to_owned(),
        display: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_properties() {
        let directive = stop_resume_directive();
        assert_eq!(directive.custom_type, "session-resumed");
        assert!(!directive.display);
        assert!(directive.content.contains("The user stopped the previous turn"));
    }
}
