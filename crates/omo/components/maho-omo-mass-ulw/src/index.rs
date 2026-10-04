use std::sync::Arc;
use maho_ext_api::*;

pub const MASS_ULW_CUSTOM_TYPE: &str = "omo-mass-ulw:skill-pointer";
pub const MASS_ULW_DISABLED_FLAG: &str = "omo-senpi-mass-ulw-disabled";

pub fn is_mass_ulw_input(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.match_indices("mass").any(|(start, _)| {
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        if lower[..start].chars().next_back().is_some_and(word) { return false; }
        let rest = lower[start + 4..].trim_start_matches(|c: char| c.is_whitespace() || c == '-');
        rest.strip_prefix("ulw").is_some_and(|tail| !tail.starts_with('-') && !tail.chars().next().is_some_and(word))
    })
}

pub fn skill_already_invoked(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.match_indices("<skill").any(|(start, _)| {
        let tail = &lower[start + 6..];
        tail.chars().next().is_some_and(char::is_whitespace) && tail.trim_start().starts_with("name=\"mass-ulw\"")
    }) || text.strip_prefix("/skill:").is_some_and(|rest| rest.split(' ').next() == Some("mass-ulw"))
}

pub fn mass_ulw_skill_pointer(skills_root: &str) -> String {
    format!("<omo-mass-ulw-pointer>The user asked for mass-ulw. Read the mass-ulw skill at {skills_root}mass-ulw/SKILL.md with the read tool and follow it: orchestrate the requested work as a dependency graph of child agents with the dag tool.</omo-mass-ulw-pointer>")
}

pub struct MassUlwComponent { pub skills_root: String }
impl Extension for MassUlwComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let content = mass_ulw_skill_pointer(&self.skills_root);
        let runtime = api.runtime.clone();
        api.on(EventKind::Input, Arc::new(move |event, _| {
            let content = content.clone(); let runtime = runtime.clone();
            Box::pin(async move {
                let ExtensionEvent::Input(input) = event else { return Ok(EventResult::None); };
                if runtime.get_flag(MASS_ULW_DISABLED_FLAG) == Some(FlagValue::Boolean(true)) || input.source == InputSource::Extension
                    || !is_mass_ulw_input(&input.text) || skill_already_invoked(&input.text) {
                    return Ok(EventResult::Input(InputEventResult::Continue));
                }
                if input.streaming_behavior.is_some() {
                    return Ok(EventResult::Input(InputEventResult::Transform { text: format!("{}\n{content}",input.text), images: None }));
                }
                // Runtime actions are deliberately accessed through the native public API.
                let api = ExtensionApi::new(LoadedExtension::new("mass-ulw", std::path::PathBuf::new(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), runtime);
                api.send_message(CustomMessage { custom_type: MASS_ULW_CUSTOM_TYPE.into(), content: vec![ToolContent::text(content)], display: false, details: None }, SendMessageOptions::default())?;
                Ok(EventResult::Input(InputEventResult::Continue))
            })
        }));
    }
}
