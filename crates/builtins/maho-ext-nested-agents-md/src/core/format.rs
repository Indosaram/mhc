use std::path::Path;
pub fn format_directory_context(absolute_path: &Path, content: &str, truncated: bool) -> String {
 let notice = if truncated { format!("\n\n[Note: Content was truncated to save context window space. For full context, please read the file directly: {}]", absolute_path.display()) } else { String::new() };
 format!("\n\n[Directory Context: {}]\n{content}{notice}", absolute_path.display())
}
