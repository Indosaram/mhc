pub fn normalize_patch_text(text:&str)->String { text.replace("\r\n","\n").replace('\r',"\n") }
pub fn strip_heredoc(input:&str)->&str {
    fn whitespace(character:char)->bool { matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }
    let mut header=input;
    if let Some(rest)=header.strip_prefix("cat") { if !rest.starts_with(whitespace) { return input; } header=rest.trim_start_matches(whitespace); }
    let Some(rest)=header.strip_prefix("<<") else { return input; };
    let rest=rest.strip_prefix(['\'','"']).unwrap_or(rest);
    let length=rest.bytes().take_while(|byte|byte.is_ascii_alphanumeric() || *byte==b'_').count(); if length==0 { return input; }
    let tag=&rest[..length]; let rest=rest[length..].strip_prefix(['\'','"']).unwrap_or(&rest[length..]);
    let newlines=rest.char_indices().take_while(|(_,character)|whitespace(*character)).filter(|(_,character)|*character=='\n').map(|(index,_)|index).collect::<Vec<_>>();
    for newline in newlines.into_iter().rev() {
        let body=&rest[newline+1..];
        for (index,_) in body.match_indices('\n') { let tail=&body[index+1..]; if tail.strip_prefix(tag).is_some_and(|suffix|suffix.chars().all(whitespace)) { return &body[..index]; } }
    }
    input
}
pub fn extract_patched_paths(text:&str)->Vec<String> {
    let normalized=normalize_patch_text(text);
    strip_heredoc(&normalized).split(['\n','\u{2028}','\u{2029}']).filter_map(|line|["*** Add File: ","*** Delete File: ","*** Update File: ","*** Move to: "].iter().find_map(|p|line.strip_prefix(p)).filter(|p|!p.is_empty()).map(str::to_owned)).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn heredoc_backtracks_header_whitespace_for_empty_body() { assert_eq!(strip_heredoc("<<PATCH\n\n\nPATCH"),""); }
    #[test] fn heredoc_accepts_trailing_newline_and_multiline_header_whitespace() { assert_eq!(strip_heredoc("cat\u{feff}<<'PATCH' \n\na\nPATCH\n"),"a"); assert_eq!(strip_heredoc("<<PATCH\na\nPATCH\n"),"a"); }
    #[test] fn normalizes_line_endings() { assert_eq!(normalize_patch_text("a\r\nb\rc"),"a\nb\nc"); }
    #[test] fn strips_heredoc() { assert_eq!(strip_heredoc("cat <<'PATCH'\na\nPATCH"),"a"); assert_eq!(strip_heredoc("<<PATCH\na\nOTHER"),"<<PATCH\na\nOTHER"); }
    #[test] fn paths_include_move() { assert_eq!(extract_patched_paths("*** Add File: a\r\n*** Move to: b"),["a","b"]); }
}
