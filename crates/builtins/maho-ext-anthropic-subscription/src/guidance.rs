use std::collections::HashSet;
use crate::executable::ExecutableSource;

#[derive(Default)]
pub struct PresetAppendDeprecation { armed_sessions: HashSet<String> }
impl PresetAppendDeprecation {
    pub fn reset(&mut self, session: Option<&str>) {
        match session { Some(session) => { self.armed_sessions.remove(session); }, None => self.armed_sessions.clear() }
    }
    pub fn guidance(&mut self, mode: &str, conflict: bool, session: &str) -> Option<String> {
        if self.armed_sessions.contains(session) || (!conflict && mode != "preset-append") { return None; }
        self.armed_sessions.insert(session.into());
        let mut parts = Vec::new();
        if mode == "preset-append" { parts.push("preset-append system-prompt mode is deprecated; `full` mode delivers the complete senpi system prompt; preset-append will be removed after one release."); }
        if conflict { parts.push("systemPromptMode wins."); }
        Some(parts.join(" "))
    }
}

pub fn version_floor_guidance(text: &str, source: Option<ExecutableSource>, executable: &str) -> Option<String> {
    let floor = regex::Regex::new(r"(?i)does not support this model; version (\S+?) or newer is required|claude_code_version_too_old").expect("version floor");
    if let Some(captures) = floor.captures(text) {
        let target = captures.get(1).map_or_else(|| "a newer Claude Code".into(), |version| format!("Claude Code {} or newer", version.as_str().trim_end_matches(['.', ',', ';', ':'])));
        return Some(match source {
            Some(ExecutableSource::Override) => format!("The Claude Code binary set by CLAUDE_CODE_EXECUTABLE ({executable}) is too old for this model. Replace it with {target}, or unset CLAUDE_CODE_EXECUTABLE to use the Claude Code senpi/omo ships."),
            Some(ExecutableSource::Bundled) => format!("The Claude Code bundled with senpi/omo ({executable}) is too old for this model; `claude update` does not change it. Update senpi/omo, install {target} as `claude` on PATH, or set CLAUDE_CODE_EXECUTABLE to {target} binary."),
            Some(ExecutableSource::Path) => format!("The Claude Code on PATH ({executable}) is too old for this model. Run `claude update` to get {target}, or set CLAUDE_CODE_EXECUTABLE to {target} binary."),
            None => format!("The Claude Code binary is too old for this model. Update senpi/omo, update the `claude` on PATH, or set CLAUDE_CODE_EXECUTABLE to {target} binary."),
        });
    }
    regex::Regex::new(r"(?i)\bmodel_not_found\b|unrecognized_model|not found for provider").expect("model missing").is_match(text).then(|| "The bundled Claude Code binary does not know this model id; update senpi/omo or set CLAUDE_CODE_EXECUTABLE to a newer Claude Code binary.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deprecation_is_once_per_session_and_reset_rearms() {
        let mut state = PresetAppendDeprecation::default();
        assert!(state.guidance("full", false, "s").is_none());
        assert!(state.guidance("preset-append", false, "s").is_some());
        assert!(state.guidance("full", true, "s").is_none());
        assert!(state.guidance("full", true, "other").is_some());
        state.reset(Some("s")); assert!(state.guidance("full", true, "s").is_some());
        state.reset(None); assert!(state.armed_sessions.is_empty());
    }
}
