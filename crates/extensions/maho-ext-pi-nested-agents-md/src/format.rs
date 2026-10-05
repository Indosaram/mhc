pub fn format_directory_context(absolute_path: &str, content: &str, truncated: bool) -> String {
    let notice = if truncated {
        format!("\n\n[Note: Content was truncated to save context window space. For full context, please read the file directly: {absolute_path}]")
    } else { String::new() };
    format!("\n\n[Directory Context: {absolute_path}]\n{content}{notice}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn untruncated_block_matches_source() { assert_eq!(format_directory_context("/abs/repo/src/AGENTS.md","# rules\nbe nice",false),"\n\n[Directory Context: /abs/repo/src/AGENTS.md]\n# rules\nbe nice"); }
    #[test] fn truncation_notice_carries_absolute_path() { assert_eq!(format_directory_context("/abs/repo/src/AGENTS.md","head",true),"\n\n[Directory Context: /abs/repo/src/AGENTS.md]\nhead\n\n[Note: Content was truncated to save context window space. For full context, please read the file directly: /abs/repo/src/AGENTS.md]"); }
    #[test] fn empty_content_keeps_header() { assert_eq!(format_directory_context("/abs/repo/src/AGENTS.md","",false),"\n\n[Directory Context: /abs/repo/src/AGENTS.md]\n"); }
    #[test] fn multiline_content_is_verbatim() { let content = "line one\nline two\nline three"; assert_eq!(format_directory_context("/abs/repo/AGENTS.md",content,false),format!("\n\n[Directory Context: /abs/repo/AGENTS.md]\n{content}")); }
}
