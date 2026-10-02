pub fn format_directory_context(absolute_path: &str, content: &str, truncated: bool) -> String {
    let notice = if truncated {
        format!("\n\n[Note: Content was truncated to save context window space. For full context, please read the file directly: {absolute_path}]")
    } else { String::new() };
    format!("\n\n[Directory Context: {absolute_path}]\n{content}{notice}")
}
