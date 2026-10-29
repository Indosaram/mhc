pub const HYPERPLAN_MODE_PROMPT: &str = strip_final_line_feed(include_str!("../prompts/mode/hyperplan.md"));
pub const TEAM_MODE_PROMPT: &str = strip_final_line_feed(include_str!("../prompts/mode/team.md"));

const fn strip_final_line_feed(prompt: &str) -> &str {
    let bytes = prompt.as_bytes();
    if let Some((&b'\n', rest)) = bytes.split_last() {
        // Removing an ASCII line feed preserves UTF-8 validity.
        match str::from_utf8(rest) { Ok(value) => value, Err(_) => prompt }
    } else { prompt }
}
