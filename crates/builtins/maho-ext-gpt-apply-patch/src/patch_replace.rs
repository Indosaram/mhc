use crate::{seek_sequence::seek_sequence_with_fuzz,text::normalize_patch_text,types::PatchChunk};
pub fn replace_chunks(content:&str,path:&str,chunks:&[PatchChunk])->Result<(String,u32),String> {
    let normalized=normalize_patch_text(content);
    let mut original:Vec<_>=normalized.split('\n').map(str::to_owned).collect();
    if original.last().is_some_and(String::is_empty) { original.pop(); }
    let mut replacements=Vec::new(); let mut line_index=0; let mut fuzz=0;
    for chunk in chunks {
        for context in &chunk.change_contexts {
            let Some((index,amount))=seek_sequence_with_fuzz(&original,std::slice::from_ref(context),line_index,false) else { return Err(format!("Failed to find context '{context}' in {path}")); };
            fuzz+=amount; line_index=index+1;
        }
        if chunk.old_lines.is_empty() {
            let index=if original.last().is_some_and(String::is_empty) { original.len()-1 } else { original.len() };
            replacements.push((index,0,chunk.new_lines.clone())); continue;
        }
        let mut pattern=chunk.old_lines.as_slice(); let mut new_lines=chunk.new_lines.as_slice();
        let mut found=seek_sequence_with_fuzz(&original,pattern,line_index,chunk.is_end_of_file);
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern=&pattern[..pattern.len()-1];
            if new_lines.last().is_some_and(String::is_empty) { new_lines=&new_lines[..new_lines.len()-1]; }
            found=seek_sequence_with_fuzz(&original,pattern,line_index,chunk.is_end_of_file);
        }
        let Some((index,amount))=found else { return Err(format!("Failed to find expected lines in {path}:\n{}",chunk.old_lines.join("\n"))); };
        fuzz+=amount; replacements.push((index,pattern.len(),new_lines.to_vec())); line_index=index+pattern.len();
    }
    replacements.sort_by_key(|replacement|std::cmp::Reverse(replacement.0));
    for (start,len,lines) in replacements { original.splice(start..start+len,lines); }
    original.push(String::new()); Ok((original.join("\n"),fuzz))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn applies_chunks_to_original_positions() { let chunks=[PatchChunk{old_lines:vec!["a".into()],new_lines:vec!["A".into(),"B".into()],..Default::default()},PatchChunk{old_lines:vec!["b".into()],new_lines:vec!["C".into()],..Default::default()}]; assert_eq!(replace_chunks("a\nb\n","file",&chunks).unwrap(),("A\nB\nC\n".into(),0)); }
    #[test] fn context_skips_to_scope() { let chunk=PatchChunk{change_contexts:vec!["class".into()],old_lines:vec!["a".into()],new_lines:vec!["b".into()],..Default::default()}; assert_eq!(replace_chunks("a\nclass\na\n","file",&[chunk]).unwrap().0,"a\nclass\nb\n"); }
    #[test] fn failed_lines() { let chunk=PatchChunk{old_lines:vec!["missing".into()],..Default::default()}; assert_eq!(replace_chunks("a","file",&[chunk]).unwrap_err(),"Failed to find expected lines in file:\nmissing"); }
    #[test] fn trailing_empty_fallback() { let chunk=PatchChunk{old_lines:vec!["a".into(),"".into()],new_lines:vec!["b".into(),"".into()],..Default::default()}; assert_eq!(replace_chunks("a\n","file",&[chunk]).unwrap().0,"b\n"); }
}
