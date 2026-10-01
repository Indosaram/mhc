use crate::{stream_utils::{is_ascii_whitespace, ScalarScanner}, types::{DetectorContext, DetectorMatch, DetectorRule, DetailValue, StreamDetector}};
pub use super::leak_context::LeakContextKind;
use super::{leak_context::StreamContextTracker, token_grammar::{CandidateOutcome, CandidateParser, ControlToken, TokenFamily}};
#[derive(Clone, Debug)]
pub struct PendingControlEvidence { pub token_id:String, pub family:TokenFamily, pub start_offset:usize, pub end_offset:usize, pub quotation_like:bool, pub gap_length:usize, pub first_payload_offset:Option<usize>, pub expires_at_offset:usize }
struct TokenRun { token_id:String, family:TokenFamily, context:LeakContextKind, first_start_offset:usize, count:u32 }
pub struct ControlLeakState { scanner:ScalarScanner, parser:CandidateParser, tracker:StreamContextTracker, run:Option<TokenRun>, whitespace_only_since_token:bool, whitespace_since_token:usize, pub pending_evidence:Option<PendingControlEvidence>, pub latched:Option<DetectorMatch>, pub current_offset:usize }
fn threshold(context:LeakContextKind)->u32 { match context { LeakContextKind::Start=>3, LeakContextKind::Normal=>4, LeakContextKind::Quotation=>8 } }
fn note_non_whitespace(state:&mut ControlLeakState,offset:usize) { if let Some(evidence)=&mut state.pending_evidence && evidence.first_payload_offset.is_none() { evidence.gap_length=state.whitespace_since_token; evidence.first_payload_offset=Some(offset); } }
fn process_ground(state:&mut ControlLeakState,value:&[u16],offset:usize) { if value.len()==1 && is_ascii_whitespace(value[0]) { if state.whitespace_since_token<33 { state.whitespace_since_token+=1; } } else { state.whitespace_only_since_token=false; note_non_whitespace(state,offset); } state.tracker.observe_ground(value); }
fn process_reject(state:&mut ControlLeakState,text:&[u16],offset:usize) { state.whitespace_only_since_token=false; note_non_whitespace(state,offset); state.tracker.observe_text(text); }
fn process_token(state:&mut ControlLeakState,token:ControlToken) {
    let context=state.tracker.classify(token.start_offset); note_non_whitespace(state,token.start_offset);
    if let Some(run)=&mut state.run && run.token_id==token.token_id && state.whitespace_only_since_token && state.whitespace_since_token<=32 { run.count+=1; } else { state.run=Some(TokenRun { token_id:token.token_id.clone(),family:token.family,context,first_start_offset:token.start_offset,count:1 }); }
    state.pending_evidence=Some(PendingControlEvidence { token_id:token.token_id,family:token.family,start_offset:token.start_offset,end_offset:token.end_offset,quotation_like:context==LeakContextKind::Quotation,gap_length:0,first_payload_offset:None,expires_at_offset:token.end_offset+2048 });
    state.whitespace_since_token=0; state.whitespace_only_since_token=true; state.tracker.observe_text(&token.text.encode_utf16().collect::<Vec<_>>());
    if let Some(active)=&state.run && active.count>=threshold(active.context) {
        let context=match active.context { LeakContextKind::Start=>"start", LeakContextKind::Normal=>"normal", LeakContextKind::Quotation=>"quotation" };
        state.latched=Some(DetectorMatch { rule:DetectorRule::ControlTokenLeak,reason:format!("{}x {} run in {context} context",active.count,active.token_id),anomaly_start_offset:active.first_start_offset,garbage_start_offset:active.first_start_offset,detail:[("tokenId".into(),DetailValue::String(active.token_id.clone())),("family".into(),DetailValue::String(active.family.as_str().into())),("occurrences".into(),DetailValue::Number(f64::from(active.count))),("context".into(),DetailValue::String(context.into()))].into() });
    }
}
pub fn corroborates_control_leak(evidence:&PendingControlEvidence,anomaly_start_offset:usize,current_offset:usize)->bool { current_offset<=evidence.expires_at_offset && evidence.gap_length<=32 && evidence.first_payload_offset==Some(anomaly_start_offset) }
pub struct ControlLeakDetector;
pub fn create_control_leak_detector()->ControlLeakDetector { ControlLeakDetector }
impl StreamDetector<ControlLeakState> for ControlLeakDetector {
    fn create_state(&self)->ControlLeakState { ControlLeakState { scanner:ScalarScanner::default(),parser:CandidateParser::default(),tracker:StreamContextTracker::default(),run:None,whitespace_only_since_token:true,whitespace_since_token:0,pending_evidence:None,latched:None,current_offset:0 } }
    fn check_delta(&self,state:&mut ControlLeakState,delta:&[u16],_context:&DetectorContext)->Option<DetectorMatch> {
        if state.latched.is_some() { return state.latched.clone(); }
        for entry in state.scanner.push(delta) {
            for outcome in state.parser.feed(&entry.value,entry.start_offset) { match outcome { CandidateOutcome::Ground { value,offset }=>process_ground(state,&value,offset), CandidateOutcome::Reject { text,start_offset }=>process_reject(state,&text,start_offset), CandidateOutcome::Token { token }=>process_token(state,token) } }
            if state.latched.is_some() { break; }
        }
        state.current_offset=state.scanner.offset(); state.latched.clone()
    }
    fn flush(&self,state:&mut ControlLeakState,_context:&DetectorContext)->Option<DetectorMatch> { state.latched.clone() }
}
#[cfg(test)] mod tests {
    use super::*; use crate::types::TtsrStreamSource;
    fn context()->DetectorContext { DetectorContext { source:TtsrStreamSource::Text,stream_key:"test".into(),generation:1 } }
    fn run(text:&str)->ControlLeakState { let detector=create_control_leak_detector(); let mut state=detector.create_state(); for unit in text.encode_utf16() { detector.check_delta(&mut state,&[unit],&context()); } state }
    #[test] fn three_control_tokens_at_start_fire() { let state=run("<|im_start|> <|im_start|> <|im_start|>"); assert_eq!(state.latched.unwrap().anomaly_start_offset,0); }
    #[test] fn normal_context_requires_four_tokens() { let quiet=run("Ordinary prose. <|end|> <|end|> <|end|>"); let loud=run("Ordinary prose. <|end|> <|end|> <|end|> <|end|>"); assert!(quiet.latched.is_none()); assert!(loud.latched.is_some()); }
    #[test] fn short_analysis_preamble_is_start_context() { let state=run("Thinking... <|sep|> <|sep|> <|sep|>"); assert_eq!(state.latched.unwrap().anomaly_start_offset,12); }
    #[test] fn sgml_and_brackets_have_start_threshold() { for text in ["<s> <s> <s>","[PAD] [PAD] [PAD]","</s> </s> </s>"] { let state=run(text); assert!(state.latched.is_some()); } }
    #[test] fn offset_thirty_two_is_normal_context() { let state=run(&format!("{}<|end|> <|end|> <|end|>"," ".repeat(32))); assert!(state.latched.is_none()); }
    #[test] fn quotations_require_eight_identical_tokens() { let quiet=run(&format!("```\n{}",["<|end|>";7].join(" "))); let loud=run(&format!("```\n{}",["<|end|>";8].join(" "))); assert!(quiet.latched.is_none()); assert!(loud.latched.is_some()); }
    #[test] fn adjacent_payload_corroborates() { let state=run("<|close|>\n!!!!"); let evidence=state.pending_evidence.unwrap(); assert_eq!(evidence.gap_length,1); assert_eq!(evidence.first_payload_offset,Some(10)); assert!(corroborates_control_leak(&evidence,10,state.current_offset)); }
    #[test] fn thirty_three_whitespace_gap_prevents_corroboration() { let state=run(&format!("<|close|>{}!"," ".repeat(33))); let evidence=state.pending_evidence.unwrap(); assert!(!corroborates_control_leak(&evidence,42,state.current_offset)); }
    #[test] fn evidence_expires_after_exact_ttl_boundary() { let state=run("<|close|>!"); let evidence=state.pending_evidence.unwrap(); assert!(corroborates_control_leak(&evidence,9,evidence.expires_at_offset)); assert!(!corroborates_control_leak(&evidence,9,evidence.expires_at_offset+1)); }
    #[test] fn no_payload_cannot_corroborate() { let state=run("<|close|>"); let evidence=state.pending_evidence.unwrap(); assert!(!corroborates_control_leak(&evidence,9,state.current_offset)); }
    #[test] fn ordinary_prose_breaks_payload_adjacency() { let state=run("<|close|> prose !!!"); let evidence=state.pending_evidence.unwrap(); assert!(!corroborates_control_leak(&evidence,16,state.current_offset)); }
    #[test] fn latched_detector_returns_same_match_on_late_delta_and_flush() { let detector=create_control_leak_detector(); let mut state=detector.create_state(); let first=detector.check_delta(&mut state,&"<s> <s> <s>".encode_utf16().collect::<Vec<_>>(),&context()); let later=detector.check_delta(&mut state,&[97],&context()); let flushed=detector.flush(&mut state,&context()); assert!(first.is_some()); assert_eq!(first,later); assert_eq!(first,flushed); }
}
