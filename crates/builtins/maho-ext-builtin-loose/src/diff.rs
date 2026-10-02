#[derive(Debug, PartialEq, Eq)]
pub struct FileInfo { pub status: String, pub status_label: String, pub file: String }
pub fn parse_status(stdout: &str) -> Vec<FileInfo> {
    stdout.split('\n').filter_map(|line| {
        if line.encode_utf16().count() < 4 { return None; }
        let mut chars = line.chars();
        let status: String = chars.by_ref().take(2).collect();
        let file = chars.as_str().trim_start().to_owned();
        let label = ['M','A','D','?','R','C'].into_iter().find(|c| status.contains(*c)).map(|c| c.to_string()).unwrap_or_else(|| {
            let trimmed = status.trim();
            if trimmed.is_empty() { "~".into() } else { trimmed.into() }
        });
        Some(FileInfo { status: label.clone(), status_label: label, file })
    }).collect()
}
pub fn windows_unsafe_cmd_path(path: &str) -> bool { path.contains(['&','|','<','>','^','%','\r','\n']) }
pub fn quote_cmd_arg(value: &str) -> String { format!("\"{}\"", value.replace('"', "\"\"")) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_priority_and_rename_text_match_porcelain_policy() {
        let files = parse_status("AM file\n?? new\n R old -> new\n!! ignored\nx\n");
        assert_eq!(files[0].status, "M");
        assert_eq!(files[1].status, "?");
        assert_eq!(files[2].file, "old -> new");
        assert_eq!(files[3].status, "!!");
        assert_eq!(files.len(), 4);
    }
    #[test]
    fn cmd_quoting_and_metacharacters_follow_upstream() {
        assert_eq!(quote_cmd_arg("a\"b"), "\"a\"\"b\"");
        assert!(windows_unsafe_cmd_path("a%b"));
        assert!(!windows_unsafe_cmd_path("a b"));
    }
}
