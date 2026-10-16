use crate::{types::ParsedPatch,streaming_parser::StreamingPatchParser};
#[derive(Default)]
pub struct StreamingRenderState { parser:Option<StreamingPatchParser>,pub input:String,pub hunks:Vec<ParsedPatch>,pub error:Option<String> }
impl StreamingRenderState {
    pub fn update(&mut self,input:&str)->&[ParsedPatch] {
        if self.parser.is_none() || !input.starts_with(&self.input) { *self=Self{parser:Some(StreamingPatchParser::default()),..Default::default()}; }
        match self.parser.as_mut().expect("initialized parser").push_delta(&input[self.input.len()..]) { Ok(hunks)=>{self.hunks=hunks; self.input=input.into(); self.error=None;},Err(error)=>self.error=Some(error) }
        &self.hunks
    }
}
pub fn format_streaming_hunks(hunks:&[ParsedPatch])->String {
    let mut lines=Vec::new();
    for hunk in hunks {
        match hunk {
            ParsedPatch::Add{file_path,content}=>{
                let diff=content.split('\n').filter(|line|!line.is_empty()).map(|line|format!("  + {line}")).collect::<Vec<_>>();
                append_file_lines(&mut lines,&format!("• Added {file_path}"),diff);
            },
            ParsedPatch::Delete{file_path}=>lines.push(format!("• Deleted {file_path}")),
            ParsedPatch::Update{file_path,move_path,chunks}=>{
                let (added,removed)=chunks.iter().fold((0,0),|(added,removed),chunk|(added+chunk.new_lines.len(),removed+chunk.old_lines.len()));
                let path=match move_path.as_deref().filter(|path|!path.is_empty()) { Some(destination)=>format!("{file_path} → {destination}"),None=>file_path.clone() };
                let operation=if move_path.as_deref().is_some_and(|path|!path.is_empty()) { "Moved" } else { "Edited" };
                let suffix=if added+removed>0 { format!(" (+{added} -{removed})") } else { String::new() };
                let header=format!("• {operation} {path}{suffix}");
                let diff=chunks.iter().flat_map(|chunk| {
                    chunk.change_contexts.iter().map(|line|format!("  @@ {line}"))
                        .chain(chunk.old_lines.iter().map(|line|format!("  - {line}")))
                        .chain(chunk.new_lines.iter().map(|line|format!("  + {line}")))
                }).collect();
                append_file_lines(&mut lines,&header,diff);
            },
        }
    }
    lines.join("\n")
}
fn append_file_lines(output:&mut Vec<String>,header:&str,lines:Vec<String>) {
    const TAIL_LINES:usize=12;
    output.push(header.into());
    if lines.len()<=TAIL_LINES { output.extend(lines); }
    else { let hidden=lines.len()-TAIL_LINES; output.push(format!("  … (+{hidden} lines above)")); output.extend(lines.into_iter().skip(hidden)); }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn streaming_prefix_reuses_state_and_replacement_resets() { let mut state=StreamingRenderState::default(); state.update("*** Begin Patch\n*** Add File: a\n"); state.update("*** Begin Patch\n*** Add File: a\n+one\n"); assert_eq!(state.hunks.len(),1); state.update("*** Begin Patch\n*** Delete File: b\n"); assert_eq!(state.hunks,[ParsedPatch::Delete{file_path:"b".into()}]); assert!(state.error.is_none()); }
    #[test] fn error_retains_previous_input_and_hunks() { let mut state=StreamingRenderState::default(); let input="*** Begin Patch\n*** Add File: a\n"; state.update(input); state.update(&format!("{input}bad\n")); assert!(state.error.is_some()); assert_eq!(state.input,input); assert_eq!(state.hunks.len(),1); }
    #[test] fn streaming_hunks_show_counts_and_bound_each_file_to_tail() {
        let patch="*** Begin Patch\n*** Update File: a\n@@\n-old\n+new\n*** End Patch";
        let hunks=crate::parser::parse_patch(patch).unwrap();
        let rendered=format_streaming_hunks(&hunks);
        assert!(rendered.starts_with("• Edited a (+1 -1)"));
        let long=(0..20).map(|line|format!("  + line {line}")).collect::<Vec<_>>();
        let mut output=Vec::new(); append_file_lines(&mut output,"• Added b (+20 -0)",long);
        assert_eq!(output.len(),14);
        assert_eq!(output[1],"  … (+8 lines above)");
        assert_eq!(output[2],"  + line 8");
        assert_eq!(output[13],"  + line 19");
    }
}
