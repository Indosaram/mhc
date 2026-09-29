//! Tool-name display normalization.

pub fn transform_tool_name(tool_name: &str) -> String {
    let trimmed = tool_name.trim();
    let special = match trimmed.to_lowercase().as_str() {
        "webfetch" => Some("WebFetch"),
        "websearch" => Some("WebSearch"),
        "todoread" => Some("TodoRead"),
        "todowrite" => Some("TodoWrite"),
        _ => None,
    };
    if let Some(mapped) = special {
        return mapped.to_string();
    }
    if trimmed.contains('-') || trimmed.contains('_') {
        return trimmed
            .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
            .filter(|word| !word.is_empty())
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first
                        .to_uppercase()
                        .chain(chars.as_str().to_lowercase().chars())
                        .collect()
                })
            })
            .collect();
    }
    let mut chars = trimmed.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}
