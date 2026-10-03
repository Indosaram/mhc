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
            ParsedPatch::Add{file_path,content}=>{ lines.push(format!("• Added {file_path}")); lines.extend(content.split('\n').filter(|line|!line.is_empty()).map(|line|format!("  + {line}"))); },
            ParsedPatch::Delete{file_path}=>lines.push(format!("• Deleted {file_path}")),
            ParsedPatch::Update{file_path,move_path,chunks}=>{
                lines.push(match move_path.as_deref().filter(|path|!path.is_empty()) { Some(destination)=>format!("• Moved {file_path} → {destination}"),None=>format!("• Edited {file_path}") });
                for chunk in chunks { lines.extend(chunk.change_contexts.iter().map(|line|format!("  @@ {line}"))); lines.extend(chunk.old_lines.iter().map(|line|format!("  - {line}"))); lines.extend(chunk.new_lines.iter().map(|line|format!("  + {line}"))); }
            },
        }
    }
    lines.join("\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn streaming_prefix_reuses_state_and_replacement_resets() { let mut state=StreamingRenderState::default(); state.update("*** Begin Patch\n*** Add File: a\n"); state.update("*** Begin Patch\n*** Add File: a\n+one\n"); assert_eq!(state.hunks.len(),1); state.update("*** Begin Patch\n*** Delete File: b\n"); assert_eq!(state.hunks,[ParsedPatch::Delete{file_path:"b".into()}]); assert!(state.error.is_none()); }
    #[test] fn error_retains_previous_input_and_hunks() { let mut state=StreamingRenderState::default(); let input="*** Begin Patch\n*** Add File: a\n"; state.update(input); state.update(&format!("{input}bad\n")); assert!(state.error.is_some()); assert_eq!(state.input,input); assert_eq!(state.hunks.len(),1); }
}
