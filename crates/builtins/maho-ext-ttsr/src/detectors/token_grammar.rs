#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenFamily { Ctrl, Sgml, Bracket }
impl TokenFamily { pub const fn as_str(self) -> &'static str { match self { Self::Ctrl => "ctrl", Self::Sgml => "sgml", Self::Bracket => "bracket" } } }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlToken { pub family: TokenFamily, pub name: String, pub token_id: String, pub text: String, pub start_offset: usize, pub end_offset: usize }
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CandidateOutcome { Ground { value: Vec<u16>, offset: usize }, Token { token: ControlToken }, Reject { text: Vec<u16>, start_offset: usize } }
#[derive(Clone, Copy, Default)]
enum ParserMode { #[default] Ground, AngleOpen, CtrlFirst, CtrlName, CtrlClose, SgmlFirst, SgmlName, BracketFirst, BracketName }
#[derive(Default)]
pub struct CandidateParser { mode: ParserMode, buffer: Vec<u16>, name_length: usize, start_offset: usize }
fn letter(value: &[u16]) -> bool { value.first().is_some_and(|v| matches!(v,65..=90 | 97..=122)) }
fn name_char(value: &[u16]) -> bool { letter(value) || value.first().is_some_and(|v| matches!(v,48..=57)) || value == [95] }
fn upper(value: &[u16]) -> bool { value.first().is_some_and(|v| matches!(v,65..=90)) }
impl CandidateParser {
    pub fn feed(&mut self, value: &[u16], offset: usize) -> Vec<CandidateOutcome> {
        match self.mode {
            ParserMode::Ground => { if value == [60] { self.begin(ParserMode::AngleOpen,value,offset); vec![] } else if value == [91] { self.begin(ParserMode::BracketFirst,value,offset); vec![] } else { vec![CandidateOutcome::Ground { value:value.to_vec(),offset }] } },
            ParserMode::AngleOpen => { if value == [124] { self.append(value,ParserMode::CtrlFirst); vec![] } else if value == [47] { self.append(value,ParserMode::SgmlFirst); vec![] } else if letter(value) { self.name_length=1; self.append(value,ParserMode::SgmlName); vec![] } else { self.reject_with(value,offset) } },
            ParserMode::CtrlFirst | ParserMode::SgmlFirst => { if letter(value) { self.name_length=1; let mode=match self.mode { ParserMode::CtrlFirst=>ParserMode::CtrlName, _=>ParserMode::SgmlName }; self.append(value,mode); vec![] } else { self.reject_with(value,offset) } },
            ParserMode::CtrlName => { if value == [124] { self.append(value,ParserMode::CtrlClose); vec![] } else if name_char(value) && self.name_length<32 { self.name_length+=1; self.buffer.extend_from_slice(value); vec![] } else { self.reject_with(value,offset) } },
            ParserMode::CtrlClose => { if value == [62] { vec![self.emit(TokenFamily::Ctrl,2,1,value,offset)] } else { self.reject_with(value,offset) } },
            ParserMode::SgmlName => { if value == [62] { vec![self.emit(TokenFamily::Sgml,1,0,value,offset)] } else if name_char(value) && self.name_length<16 { self.name_length+=1; self.buffer.extend_from_slice(value); vec![] } else { self.reject_with(value,offset) } },
            ParserMode::BracketFirst => { if upper(value) { self.name_length=1; self.append(value,ParserMode::BracketName); vec![] } else { self.reject_with(value,offset) } },
            ParserMode::BracketName => { if value == [93] { if self.name_length<2 { self.reject_with(value,offset) } else { vec![self.emit(TokenFamily::Bracket,1,0,value,offset)] } } else if (upper(value) || value.first().is_some_and(|v| matches!(v,48..=57)) || value == [95]) && self.name_length<16 { self.name_length+=1; self.buffer.extend_from_slice(value); vec![] } else { self.reject_with(value,offset) } },
        }
    }
    pub fn pending_length(&self) -> usize { match self.mode { ParserMode::Ground=>0, _=>self.buffer.len() } }
    fn begin(&mut self, mode: ParserMode, value: &[u16], offset: usize) { self.mode=mode; self.buffer=value.to_vec(); self.name_length=0; self.start_offset=offset; }
    fn append(&mut self, value: &[u16], mode: ParserMode) { self.buffer.extend_from_slice(value); self.mode=mode; }
    fn emit(&mut self, family: TokenFamily, name_start: usize, trim_end: usize, closer: &[u16], offset: usize) -> CandidateOutcome {
        let name=String::from_utf16_lossy(&self.buffer[name_start..self.buffer.len()-trim_end]);
        self.buffer.extend_from_slice(closer);
        let token=ControlToken { family, token_id:format!("{}:{name}",family.as_str()), name, text:String::from_utf16_lossy(&self.buffer), start_offset:self.start_offset,end_offset:offset+1 };
        self.mode=ParserMode::Ground; self.buffer.clear(); self.name_length=0; CandidateOutcome::Token { token }
    }
    fn reject_with(&mut self,value: &[u16],offset: usize) -> Vec<CandidateOutcome> {
        let mut outcomes=vec![CandidateOutcome::Reject { text:std::mem::take(&mut self.buffer),start_offset:self.start_offset }]; self.mode=ParserMode::Ground; self.name_length=0;
        if value == [60] { self.begin(ParserMode::AngleOpen,value,offset); } else if value == [91] { self.begin(ParserMode::BracketFirst,value,offset); } else { outcomes.push(CandidateOutcome::Ground { value:value.to_vec(),offset }); }
        outcomes
    }
}
#[cfg(test)] mod tests {
    use super::*;
    fn parse(text: &str) -> Vec<CandidateOutcome> { let mut parser=CandidateParser::default(); text.encode_utf16().enumerate().flat_map(|(offset,unit)| parser.feed(&[unit],offset)).collect() }
    #[test] fn control_token_carries_name_and_offsets() { let result=parse("<|im_start|>"); assert!(matches!(&result[0],CandidateOutcome::Token { token } if token.name=="im_start" && token.token_id=="ctrl:im_start" && token.start_offset==0 && token.end_offset==12)); }
    #[test] fn closing_sgml_preserves_slash_in_name() { let result=parse("</think>"); assert!(matches!(&result[0],CandidateOutcome::Token { token } if token.name=="/think")); }
    #[test] fn bracket_requires_uppercase_and_two_characters() { let accepted=parse("[INST]"); let short=parse("[A]"); let lowercase=parse("[inst]"); assert!(matches!(&accepted[0],CandidateOutcome::Token { .. })); assert!(matches!(&short[0],CandidateOutcome::Reject { .. })); assert!(matches!(&lowercase[0],CandidateOutcome::Reject { .. })); }
    #[test] fn rejected_candidate_restarts_on_new_opening() { let result=parse("<|bad<|good|>"); assert!(matches!(&result[0],CandidateOutcome::Reject { .. })); assert!(matches!(&result[1],CandidateOutcome::Token { token } if token.name=="good" && token.start_offset==5)); }
    #[test] fn oversized_control_name_is_rejected() { let result=parse(&format!("<|{}|>","a".repeat(33))); assert!(matches!(&result[0],CandidateOutcome::Reject { .. })); }
    #[test] fn pending_length_tracks_partial_token() { let mut parser=CandidateParser::default(); parser.feed(&[60],0); parser.feed(&[124],1); let result=parser.pending_length(); assert_eq!(result,2); }
    #[test] fn ground_preserves_unpaired_surrogate() { let mut parser=CandidateParser::default(); let result=parser.feed(&[0xd800],9); assert_eq!(result,vec![CandidateOutcome::Ground { value:vec![0xd800],offset:9 }]); }
}
