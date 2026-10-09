use maho_omo_bundled_skills::contributed_skill::ContributedSkill;
use senpi_desktop_tool::skill::COMPUTER_SKILL_NAME;
use serde_json::Value;

pub fn skill_status_line(skill: Option<&ContributedSkill>) -> String {
    match skill {
        Some(ContributedSkill::Yielded { owner_path }) => {
            let r#where = match owner_path {
                Some(p) => format!(" ({p})"),
                None => String::new(),
            };
            format!("\nskill: your own {COMPUTER_SKILL_NAME} skill is active in place of the built-in guide{where}")
        }
        _ => String::new(),
    }
}

pub fn tool_activated_names(payload: &Value) -> Vec<String> {
    if let Value::Object(map) = payload
        && let Some(Value::Array(names)) = map.get("toolNames").or_else(|| map.get("tool_names")) {
        return names
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect();
    }
    Vec::new()
}
