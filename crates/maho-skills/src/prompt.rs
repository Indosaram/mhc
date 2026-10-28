//! Removing disabled skills from the system prompt's skill listing (spec §3).

/// Removes each `<skill>` entry of an `<available_skills>` block whose `<name>` is in `disabled`.
/// Every other byte of `prompt` is kept. Returns `None` when nothing was removed.
pub fn filter_prompt(prompt: &str, disabled: &[String]) -> Option<String> {
    if disabled.is_empty() || !prompt.contains("<available_skills>") {
        return None;
    }
    let mut out = String::with_capacity(prompt.len());
    let mut in_block = false;
    let mut entry: Option<String> = None;
    let mut removed = false;
    for line in prompt.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(buffer) = entry.as_mut() {
            buffer.push_str(line);
            if trimmed == "</skill>" {
                let buffer = entry.take().unwrap_or_default();
                if entry_name(&buffer).is_some_and(|name| disabled.iter().any(|d| d == name)) {
                    removed = true;
                } else {
                    out.push_str(&buffer);
                }
            }
            continue;
        }
        if trimmed == "<available_skills>" {
            in_block = true;
        } else if trimmed == "</available_skills>" {
            in_block = false;
        } else if in_block && trimmed == "<skill>" {
            entry = Some(line.to_owned());
            continue;
        }
        out.push_str(line);
    }
    if let Some(unterminated) = entry {
        out.push_str(&unterminated);
    }
    removed.then_some(out)
}

fn entry_name(entry: &str) -> Option<&str> {
    let start = entry.find("<name>")? + "<name>".len();
    let end = entry[start..].find("</name>")? + start;
    Some(entry[start..end].trim())
}
