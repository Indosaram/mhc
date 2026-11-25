//! Initial skills injected into newly created threads (`thread/initial-skills.ts`).

use std::collections::HashSet;
use std::path::Path;

use serde_json::Value;

pub const INITIAL_SKILLS_CONTEXT_KEY: &str = "initial_skills";
pub const INITIAL_SKILLS_ENTRY: &str = "omo-initial-skills";

fn is_valid_skill_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9')
        && bytes
            .iter()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialSkillsCheck {
    Ok { names: Vec<String> },
    Invalid { names: Vec<String> },
}

pub fn check_initial_skills(names: &[String]) -> InitialSkillsCheck {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for name in names {
        if seen.insert(name.clone()) {
            unique.push(name.clone());
        }
    }
    let invalid: Vec<String> = unique
        .iter()
        .filter(|n| !is_valid_skill_name(n))
        .cloned()
        .collect();
    if invalid.is_empty() {
        InitialSkillsCheck::Ok { names: unique }
    } else {
        InitialSkillsCheck::Invalid { names: invalid }
    }
}

pub fn names_from_value(val: &Value) -> Option<Vec<String>> {
    let parsed: Value = match val {
        Value::String(s) => serde_json::from_str(s).ok()?,
        other => other.clone(),
    };
    let arr = parsed.as_array()?;
    let names: Vec<String> = arr
        .iter()
        .filter_map(Value::as_str)
        .filter(|s| is_valid_skill_name(s))
        .map(str::to_owned)
        .collect();
    (!names.is_empty()).then_some(names)
}

pub fn skill_body_without_frontmatter(path: &Path) -> Result<String, std::io::Error> {
    let text = std::fs::read_to_string(path)?;
    let trimmed = if let Some(rest) = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"))
    {
        if let Some(end_idx) = rest.find("\n---").or_else(|| rest.find("\r\n---")) {
            let after_dash = &rest[end_idx..];
            if let Some(nl) = after_dash.find('\n') {
                let after = &after_dash[nl + 1..];
                if let Some(nl2) = after.find('\n') {
                    &after[nl2 + 1..]
                } else {
                    ""
                }
            } else {
                &text
            }
        } else {
            &text
        }
    } else {
        &text
    };
    Ok(trimmed.trim().to_owned())
}
