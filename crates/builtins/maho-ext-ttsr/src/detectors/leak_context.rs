use crate::stream_utils::is_ascii_whitespace;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeakContextKind { Start, Normal, Quotation }
#[derive(Default)]
pub struct StreamContextTracker { head:Vec<u16>, window:Vec<u16>, tick_char:Option<u16>, tick_count:usize, in_fence:bool, fence_char:Option<u16>, in_inline:bool, line_indent:usize, line_has_content:bool, line_is_blockquote:bool, line_is_indented:bool, last_non_ws:Vec<u16> }
impl StreamContextTracker {
    pub fn observe_ground(&mut self,value:&[u16]) {
        if let Some(tick)=self.tick_char {
            if value==[tick] { self.tick_count+=1; self.append_text(value); self.last_non_ws=value.to_vec(); self.line_has_content=true; return; }
            self.resolve_ticks();
        }
        if value==[96] || value==[126] { self.tick_char=value.first().copied(); self.tick_count=1; self.append_text(value); self.last_non_ws=value.to_vec(); self.line_has_content=true; return; }
        if value==[10] { self.line_indent=0; self.line_has_content=false; self.line_is_blockquote=false; self.line_is_indented=false; self.append_text(value); return; }
        let whitespace=value.len()==1 && value.first().is_some_and(|v| is_ascii_whitespace(*v));
        if !self.line_has_content {
            if value==[32] { self.line_indent+=1; } else if value==[9] { self.line_is_indented=true; } else if !whitespace {
                self.line_has_content=true;
                if self.line_indent>=4 { self.line_is_indented=true; }
                if value==[62] { self.line_is_blockquote=true; }
            }
        }
        if !whitespace { self.last_non_ws=value.to_vec(); }
        self.append_text(value);
    }
    pub fn observe_text(&mut self,text:&[u16]) {
        if text.is_empty() { return; }
        if self.tick_char.is_some() { self.resolve_ticks(); }
        self.line_has_content=true; self.last_non_ws=text[text.len()-1..].to_vec(); self.append_text(text);
    }
    pub fn classify(&self,start_offset:usize) -> LeakContextKind {
        if self.in_fence || self.in_inline || self.line_is_blockquote || self.line_is_indented || self.last_non_ws==[34] || self.last_non_ws==[39] { return LeakContextKind::Quotation; }
        let tail=String::from_utf16_lossy(&self.window[self.window.len().saturating_sub(128)..]).to_lowercase();
        if ["token","tokens","tokenizer","delimiter","special","literal","example","documentation","sequence","sentinel","vocabulary"].iter().any(|word| tail.contains(word)) { return LeakContextKind::Quotation; }
        if start_offset<32 && self.head[..start_offset.min(self.head.len())].iter().all(|v| is_ascii_whitespace(*v)) { return LeakContextKind::Start; }
        if start_offset<160 && start_offset<=self.head.len() {
            let raw=String::from_utf16_lossy(&self.head[..start_offset]);
            let trimmed=raw.trim_matches(|c| matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'));
            if trimmed.encode_utf16().count()<=96 && ["Thinking","Reasoning","Analysis"].contains(&trimmed.trim_end_matches(['.',':'])) { return LeakContextKind::Start; }
        }
        LeakContextKind::Normal
    }
    fn resolve_ticks(&mut self) {
        let count=self.tick_count; let tick=self.tick_char.take(); self.tick_count=0;
        if count>=3 { if !self.in_fence { self.in_fence=true; self.fence_char=tick; } else if self.fence_char==tick { self.in_fence=false; self.fence_char=None; } return; }
        if tick==Some(96) && !self.in_fence { self.in_inline = !self.in_inline; }
    }
    fn append_text(&mut self,text:&[u16]) { if self.head.len()<160 { self.head.extend(text.iter().take(160-self.head.len())); } self.window.extend_from_slice(text); if self.window.len()>320 { self.window.drain(..self.window.len()-160); } }
}
#[cfg(test)] mod tests {
    use super::*;
    fn tracker(text:&str)->StreamContextTracker { let mut tracker=StreamContextTracker::default(); for unit in text.encode_utf16() { tracker.observe_ground(&[unit]); } tracker }
    #[test] fn leading_whitespace_is_start_context() { let tracker=tracker(" \n "); let result=tracker.classify(3); assert_eq!(result,LeakContextKind::Start); }
    #[test] fn analysis_preamble_is_start_context() { let tracker=tracker("Analysis: "); let result=tracker.classify(10); assert_eq!(result,LeakContextKind::Start); }
    #[test] fn regular_prose_is_normal_context() { let tracker=tracker("Continuing implementation now. "); let result=tracker.classify(31); assert_eq!(result,LeakContextKind::Normal); }
    #[test] fn discussion_vocabulary_suppresses_token_detection() { let tracker=tracker("Here is an example of "); let result=tracker.classify(22); assert_eq!(result,LeakContextKind::Quotation); }
    #[test] fn fenced_code_is_quotation_context() { let tracker=tracker("```\n"); let result=tracker.classify(4); assert_eq!(result,LeakContextKind::Quotation); }
    #[test] fn inline_code_is_quotation_context() { let tracker=tracker("` "); let result=tracker.classify(2); assert_eq!(result,LeakContextKind::Quotation); }
    #[test] fn indentation_is_quotation_context() { let tracker=tracker("    code "); let result=tracker.classify(9); assert_eq!(result,LeakContextKind::Quotation); }
    #[test] fn blockquote_is_quotation_context() { let tracker=tracker("> quoted "); let result=tracker.classify(9); assert_eq!(result,LeakContextKind::Quotation); }
}
