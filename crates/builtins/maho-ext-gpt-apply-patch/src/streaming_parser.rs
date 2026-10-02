use crate::{text::{normalize_patch_text,strip_heredoc},types::{ParsedPatch,PatchChunk}};
#[derive(Clone,Copy,Default,PartialEq,Eq)]
enum Mode { #[default] NotStarted,Started,Add,Delete,Update,Ended }
#[derive(Default)]
pub struct StreamingPatchParser { line_buffer:String,mode:Mode,hunks:Vec<ParsedPatch> }
impl StreamingPatchParser {
    pub fn push_delta(&mut self,delta:&str)->Result<Vec<ParsedPatch>,String> {
        for c in normalize_patch_text(delta).chars() {
            if c=='\n' { let line=std::mem::take(&mut self.line_buffer); self.process_line(&line)?; } else { self.line_buffer.push(c); }
        }
        Ok(self.hunks.clone())
    }
    pub fn finish(&mut self)->Result<Vec<ParsedPatch>,String> {
        if !self.line_buffer.is_empty() {
            let line=std::mem::take(&mut self.line_buffer);
            if line.trim()=="*** End Patch" { self.ensure_update_not_empty(line.trim())?; self.mode=Mode::Ended; } else { self.process_line(&line)?; }
        }
        if self.mode!=Mode::Ended { return Err("The last line of the patch must be '*** End Patch'".into()); }
        Ok(self.hunks.clone())
    }
    fn ensure_update_not_empty(&self,line:&str)->Result<(),String> {
        if let Some(ParsedPatch::Update{file_path,chunks,..})=self.hunks.last() {
            if chunks.is_empty() && self.mode==Mode::Update { return Err(format!("Update file hunk for path '{file_path}' is empty")); }
            if let Some(chunk)=chunks.last() && chunk.old_lines.is_empty() && chunk.new_lines.is_empty() && chunk.change_contexts.is_empty() {
                if line=="*** End Patch" { return Err("Update hunk does not contain any lines".into()); }
                return Err(format!("Unexpected line found in update hunk: '{line}'"));
            }
        }
        Ok(())
    }
    fn handle_header(&mut self,line:&str)->Result<bool,String> {
        if line=="*** End Patch" { self.ensure_update_not_empty(line)?; self.mode=Mode::Ended; return Ok(true); }
        if let Some(file_path)=line.strip_prefix("*** Add File: ") { self.ensure_update_not_empty(line)?; self.hunks.push(ParsedPatch::Add{file_path:file_path.into(),content:String::new()}); self.mode=Mode::Add; return Ok(true); }
        if let Some(file_path)=line.strip_prefix("*** Delete File: ") { self.ensure_update_not_empty(line)?; self.hunks.push(ParsedPatch::Delete{file_path:file_path.into()}); self.mode=Mode::Delete; return Ok(true); }
        if let Some(file_path)=line.strip_prefix("*** Update File: ") { self.ensure_update_not_empty(line)?; self.hunks.push(ParsedPatch::Update{file_path:file_path.into(),move_path:None,chunks:Vec::new()}); self.mode=Mode::Update; return Ok(true); }
        Ok(false)
    }
    fn current_chunk(&mut self)->Result<&mut PatchChunk,String> {
        let Some(ParsedPatch::Update{chunks,..})=self.hunks.last_mut() else { return Err("Internal parser state error: expected update hunk".into()); };
        if chunks.last().is_none_or(|c|c.is_end_of_file) { chunks.push(PatchChunk::default()); }
        chunks.last_mut().ok_or_else(||"Internal parser state error: expected update chunk".into())
    }
    fn process_line(&mut self,line:&str)->Result<(),String> {
        match self.mode {
            Mode::NotStarted=> { if strip_heredoc(line).trim()=="*** Begin Patch" { self.mode=Mode::Started; Ok(()) } else { Err("The first line of the patch must be '*** Begin Patch'".into()) } },
            Mode::Started|Mode::Delete=> { if self.handle_header(line.trim())? { Ok(()) } else { Err(format!("'{}' is not a valid hunk header",line.trim())) } },
            Mode::Add=> {
                if self.handle_header(line.trim())? { return Ok(()); }
                if let Some(value)=line.strip_prefix('+') && let Some(ParsedPatch::Add{content,..})=self.hunks.last_mut() { content.push_str(value); content.push('\n'); return Ok(()); }
                Err(format!("'{}' is not a valid hunk header",line.trim()))
            },
            Mode::Update=>self.process_update(line),
            Mode::Ended=>Ok(()),
        }
    }
    fn process_update(&mut self,line:&str)->Result<(),String> {
        let update_line=line.trim_end();
        if self.handle_header(update_line)? { return Ok(()); }
        if let Some(ParsedPatch::Update{chunks,move_path,..})=self.hunks.last_mut() && chunks.is_empty() && move_path.as_deref().is_none_or(str::is_empty) && let Some(path)=update_line.strip_prefix("*** Move to: ") { *move_path=Some(path.into()); return Ok(()); }
        if update_line=="@@" { return Ok(()); }
        if let Some(context)=update_line.strip_prefix("@@ ") { self.current_chunk()?.change_contexts.push(context.into()); return Ok(()); }
        if update_line=="*** End of File" {
            let chunk=self.current_chunk()?;
            if chunk.old_lines.is_empty() && chunk.new_lines.is_empty() { return Err("Update hunk does not contain any lines".into()); }
            chunk.is_end_of_file=true; return Ok(());
        }
        let chunk=self.current_chunk()?;
        if let Some(value)=line.strip_prefix(' ') { chunk.old_lines.push(value.into()); chunk.new_lines.push(value.into()); }
        else if let Some(value)=line.strip_prefix('-') { chunk.old_lines.push(value.into()); }
        else if let Some(value)=line.strip_prefix('+') { chunk.new_lines.push(value.into()); }
        else if line.is_empty() { chunk.old_lines.push(String::new()); chunk.new_lines.push(String::new()); }
        else { return Err(format!("Unexpected line found in update hunk: '{line}'")); }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn split_deltas() { let mut p=StreamingPatchParser::default(); assert!(p.push_delta("*** Begin Pa").unwrap().is_empty()); p.push_delta("tch\n*** Add File: a\n+he").unwrap(); p.push_delta("llo\n*** End Patch").unwrap(); assert_eq!(p.finish().unwrap(),vec![ParsedPatch::Add{file_path:"a".into(),content:"hello\n".into()}]); }
    #[test] fn snapshot_is_detached() { let mut p=StreamingPatchParser::default(); let snapshot=p.push_delta("*** Begin Patch\n*** Add File: a\n").unwrap(); p.push_delta("+new\n").unwrap(); assert!(matches!(&snapshot[0],ParsedPatch::Add{content,..} if content.is_empty())); }
    #[test] fn missing_end() { assert!(StreamingPatchParser::default().finish().is_err()); }
    #[test] fn character_stream_matches_strict_parser_after_finish() { let patch="*** Begin Patch\n*** Add File: docs/release-notes.md\n+# Release notes\n+\n+- Stream apply_patch progress.\n*** Update File: src/config.ts\n@@ Config\n-const interval = 500;\n+const interval = 250;\n*** Delete File: src/old.ts\n*** End Patch"; let mut parser=StreamingPatchParser::default(); for character in patch.chars() { parser.push_delta(&character.to_string()).unwrap(); } assert_eq!(parser.finish().unwrap(),crate::parser::parse_patch(patch).unwrap()); }
    #[test] fn wrong_first_line() { assert!(StreamingPatchParser::default().push_delta("bad\n").is_err()); }
    #[test] fn empty_update() { assert!(StreamingPatchParser::default().push_delta("*** Begin Patch\n*** Update File: a\n*** End Patch\n").is_err()); }
    #[test] fn preserves_source_move_only_rejection() { let mut p=StreamingPatchParser::default(); p.push_delta("*** Begin Patch\n*** Update File: a\n*** Move to: b\n").unwrap(); assert!(p.push_delta("*** End Patch\n").is_err()); }
    #[test] fn end_of_file_requires_lines() { assert!(StreamingPatchParser::default().push_delta("*** Begin Patch\n*** Update File: a\n*** End of File\n").is_err()); }
    #[test] fn update_lines() { let mut p=StreamingPatchParser::default(); p.push_delta("*** Begin Patch\n*** Update File: a\n@@ scope\n-old\n+new\n*** End of File\n*** End Patch\n").unwrap(); let result=p.finish().unwrap(); let ParsedPatch::Update{chunks,..}=&result[0] else { panic!() }; assert_eq!(chunks[0].old_lines,["old"]); assert_eq!(chunks[0].new_lines,["new"]); assert!(chunks[0].is_end_of_file); }
}
