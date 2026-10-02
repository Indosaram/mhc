use crate::{text::{normalize_patch_text,strip_heredoc},types::{ParsedPatch,PatchChunk}};
fn js_whitespace(character:char)->bool { matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }

fn parse_chunk(lines:&[&str],mut index:usize)->Result<(PatchChunk,usize),String> {
    let mut chunk=PatchChunk::default();
    while let Some(line)=lines.get(index) {
        if *line=="@@" { index+=1; } else if let Some(context)=line.strip_prefix("@@ ") { chunk.change_contexts.push(context.into()); index+=1; } else { break; }
    }
    let mut parsed=0usize;
    while let Some(line)=lines.get(index) {
        if *line=="*** End of File" { if parsed==0 { return Err("Update hunk does not contain any lines".into()); } chunk.is_end_of_file=true; index+=1; break; }
        if line.starts_with("@@") || line.starts_with("*** ") { break; }
        if line.is_empty() { chunk.old_lines.push(String::new()); chunk.new_lines.push(String::new()); }
        else if let Some(value)=line.strip_prefix(' ') { chunk.old_lines.push(value.into()); chunk.new_lines.push(value.into()); }
        else if let Some(value)=line.strip_prefix('-') { chunk.old_lines.push(value.into()); }
        else if let Some(value)=line.strip_prefix('+') { chunk.new_lines.push(value.into()); }
        else if parsed>0 { break; }
        else { return Err(format!("Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)")); }
        parsed+=1; index+=1;
    }
    if parsed==0 { return Err("Update hunk does not contain any lines".into()); }
    Ok((chunk,index))
}
pub fn parse_patch(text:&str)->Result<Vec<ParsedPatch>,String> {
    let normalized=normalize_patch_text(text);
    let normalized=strip_heredoc(normalized.trim_matches(js_whitespace)).trim_matches(js_whitespace);
    let lines:Vec<_>=normalized.split('\n').collect();
    if lines.first().map(|s|s.trim_matches(js_whitespace))!=Some("*** Begin Patch") || lines.last().map(|s|s.trim_matches(js_whitespace))!=Some("*** End Patch") { return Err("Invalid patch format: expected *** Begin Patch ... *** End Patch envelope".into()); }
    let body=&lines[..lines.len()-1];
    let mut patches=Vec::new(); let mut index=1;
    while index<body.len() {
        let line=body[index];
        if !line.starts_with("*** ") { index+=1; continue; }
        if let Some(path)=line.strip_prefix("*** Add File: ") {
            index+=1; let mut content=Vec::new();
            while index<body.len() && !body[index].starts_with("*** ") {
                let Some(value)=body[index].strip_prefix('+') else { return Err("Invalid patch format: Add File lines must start with '+'".into()); };
                content.push(value); index+=1;
            }
            patches.push(ParsedPatch::Add{file_path:path.into(),content:if content.is_empty() { String::new() } else { format!("{}\n",content.join("\n")) }});
        } else if let Some(path)=line.strip_prefix("*** Delete File: ") { patches.push(ParsedPatch::Delete{file_path:path.into()}); index+=1; }
        else if let Some(path)=line.strip_prefix("*** Update File: ") {
            index+=1;
            let move_path=body.get(index).and_then(|line|line.strip_prefix("*** Move to: ")).map(str::to_owned);
            if move_path.is_some() { index+=1; }
            let mut chunks=Vec::new();
            while index<body.len() {
                let line=body[index];
                if line.trim_matches(js_whitespace).is_empty() { index+=1; continue; }
                if line.starts_with("*** ") { break; }
                if !line.starts_with("@@") && !chunks.is_empty() { return Err(format!("Expected update hunk to start with a @@ context marker, got: '{line}'")); }
                let (chunk,next)=parse_chunk(body,index)?; chunks.push(chunk); index=next;
            }
            if chunks.is_empty() && move_path.as_deref().is_none_or(str::is_empty) { return Err(format!("Update file hunk for path '{path}' is empty")); }
            patches.push(ParsedPatch::Update{file_path:path.into(),move_path,chunks});
        } else { return Err(format!("'{line}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'")); }
    }
    Ok(patches)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn envelope_trims_bom_but_rejects_next_line_character() { assert_eq!(parse_patch("\u{feff}*** Begin Patch\n*** Delete File: a\n*** End Patch\u{feff}").unwrap(),vec![ParsedPatch::Delete{file_path:"a".into()}]); assert!(parse_patch("\u{0085}*** Begin Patch\n*** End Patch").is_err()); }
    #[test] fn add_and_delete() { assert_eq!(parse_patch("*** Begin Patch\n*** Add File: a\n+hello\n*** Delete File: b\n*** End Patch").unwrap(),vec![ParsedPatch::Add{file_path:"a".into(),content:"hello\n".into()},ParsedPatch::Delete{file_path:"b".into()}]); }
    #[test] fn envelope_error() { assert_eq!(parse_patch("invalid").unwrap_err(),"Invalid patch format: expected *** Begin Patch ... *** End Patch envelope"); }
    #[test] fn add_requires_prefix() { assert_eq!(parse_patch("*** Begin Patch\n*** Add File: a\nbad\n*** End Patch").unwrap_err(),"Invalid patch format: Add File lines must start with '+'"); }
    #[test] fn move_only() { assert!(matches!(&parse_patch("*** Begin Patch\n*** Update File: a\n*** Move to: b\n*** End Patch").unwrap()[0],ParsedPatch::Update{chunks,..} if chunks.is_empty())); }
    #[test] fn empty_update_errors() { assert!(parse_patch("*** Begin Patch\n*** Update File: a\n*** End Patch").is_err()); }
    #[test] fn context_and_eof() { let p=parse_patch("*** Begin Patch\n*** Update File: a\n@@ class X\n@@ method\n-old\n+new\n*** End of File\n*** End Patch").unwrap(); let ParsedPatch::Update{chunks,..}=&p[0] else { panic!() }; assert_eq!(chunks[0].change_contexts,["class X","method"]); assert!(chunks[0].is_end_of_file); }
    #[test] fn crlf_heredoc() { assert_eq!(parse_patch("<<'PATCH'\r\n*** Begin Patch\r\n*** Delete File: a\r\n*** End Patch\r\nPATCH").unwrap(),vec![ParsedPatch::Delete{file_path:"a".into()}]); }
}
