pub fn normalize_patch_text(text:&str)->String { text.replace("\r\n","\n").replace('\r',"\n") }
pub fn strip_heredoc(input:&str)->&str {
    let Some((header,rest))=input.split_once('\n') else { return input; };
    let header=header.strip_prefix("cat").filter(|h|h.starts_with(char::is_whitespace)).map(str::trim_start).unwrap_or(header);
    let Some(tag)=header.strip_prefix("<<") else { return input; };
    let tag=tag.trim_end();
    let tag=tag.strip_prefix(['\'','"']).unwrap_or(tag);
    let tag=tag.strip_suffix(['\'','"']).unwrap_or(tag);
    if tag.is_empty() || !tag.chars().all(|c|c.is_ascii_alphanumeric() || c=='_') { return input; }
    let Some((body,end))=rest.rsplit_once('\n') else { return input; };
    if end.trim_end()==tag { body } else { input }
}
pub fn extract_patched_paths(text:&str)->Vec<String> {
    let normalized=normalize_patch_text(text);
    strip_heredoc(&normalized).split('\n').filter_map(|line|["*** Add File: ","*** Delete File: ","*** Update File: ","*** Move to: "].iter().find_map(|p|line.strip_prefix(p)).filter(|p|!p.is_empty()).map(str::to_owned)).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn normalizes_line_endings() { assert_eq!(normalize_patch_text("a\r\nb\rc"),"a\nb\nc"); }
    #[test] fn strips_heredoc() { assert_eq!(strip_heredoc("cat <<'PATCH'\na\nPATCH"),"a"); assert_eq!(strip_heredoc("<<PATCH\na\nOTHER"),"<<PATCH\na\nOTHER"); }
    #[test] fn paths_include_move() { assert_eq!(extract_patched_paths("*** Add File: a\r\n*** Move to: b"),["a","b"]); }
}
