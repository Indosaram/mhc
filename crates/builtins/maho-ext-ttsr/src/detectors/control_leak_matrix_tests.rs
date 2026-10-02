use super::*;
use crate::types::TtsrStreamSource;
const PROSE:&str="We reviewed the failing render together and compared both panels side by side. The drift came from a stale cache entry, so the fix invalidates on write rather than on read. After the change the footer stayed stable across resizes.";
fn matrix(text:&str,mut check:impl FnMut(&ControlLeakState)) {
    let units:Vec<_>=text.encode_utf16().collect();
    let detector=create_control_leak_detector();
    let context=DetectorContext { source:TtsrStreamSource::Thinking,stream_key:"fixture".into(),generation:1 };
    for split in 0..=units.len() {
        let mut state=detector.create_state();
        detector.check_delta(&mut state,&units[..split],&context);
        detector.check_delta(&mut state,&units[split..],&context);
        assert_eq!(detector.flush(&mut state,&context),state.latched);
        check(&state);
    }
    let mut state=detector.create_state();
    for unit in units { detector.check_delta(&mut state,&[unit],&context); }
    check(&state);
}
fn repeated(token:&str,count:usize,gap:&str)->String { vec![token;count].join(gap) }
fn positive(prefix:&str,token:&str,count:usize,gap:&str,id:&str,kind:&str) {
    matrix(&format!("{prefix}{}",repeated(token,count,gap)),|state| {
        let found=state.latched.as_ref().unwrap();
        assert_eq!(found.anomaly_start_offset,prefix.encode_utf16().count());
        assert_eq!(found.garbage_start_offset,found.anomaly_start_offset);
        assert_eq!(found.detail["tokenId"],DetailValue::String(id.into()));
        assert_eq!(found.detail["occurrences"],DetailValue::Number(count as f64));
        assert_eq!(found.detail["context"],DetailValue::String(kind.into()));
    });
}
fn negative(text:&str) { matrix(text,|state|assert!(state.latched.is_none(),"{text}")); }
#[test] fn upstream_start_positive_families_and_preambles() {
    for (prefix,token,id) in [("Thinking... ","<|sep|>","ctrl:sep"),("","<|im_start|>","ctrl:im_start"),("Reasoning: ","<|eot_id|>","ctrl:eot_id"),("","<|fim_suffix|>","ctrl:fim_suffix"),("","<s>","sgml:s"),("Thinking.\n","</s>","sgml:/s"),("Analysis... ","[UNK]","bracket:UNK")] {
        positive(prefix,token,3," ",id,"start");
    }
}
#[test] fn upstream_normal_positive_families_and_whitespace_gaps() {
    let prefix=format!("{PROSE} ");
    for (token,id,gap) in [("<|endoftext|>","ctrl:endoftext"," "),("<|fim_prefix|>","ctrl:fim_prefix"," "),("<|fim_middle|>","ctrl:fim_middle","\t\n"),("<pad>","sgml:pad"," "),("[PAD]","bracket:PAD"," "),("[CLS]","bracket:CLS"," "),("[SEP]","bracket:SEP"," ")] { positive(&prefix,token,4,gap,id,"normal"); }
    let long="The migration path stayed compatible because every reader fell back to the legacy shape when the new field was missing. ".repeat(17);
    positive(&format!("{long}\n"),"<|im_end|>",4,"\n","ctrl:im_end","normal");
}
#[test] fn upstream_start_boundaries_are_split_invariant() {
    positive(&" ".repeat(31),"<|pad_left|>",3," ","ctrl:pad_left","start");
    negative(&format!("{}{}"," ".repeat(32),repeated("<|pad_left|>",3," ")));
    positive(&" ".repeat(32),"<|pad_left|>",4," ","ctrl:pad_left","normal");
    positive(&format!("Thinking:{}"," ".repeat(150)),"<|sep|>",3," ","ctrl:sep","start");
    negative(&format!("Thinking:{}{}"," ".repeat(151),repeated("<|sep|>",3," ")));
    negative(&format!("Thinking about the answer. {}",repeated("<|eot_id|>",3," ")));
}
#[test] fn upstream_grammar_length_and_name_acceptance() {
    let names=[(format!("<|a{}|>","b".repeat(31)),format!("ctrl:a{}","b".repeat(31))),(format!("<a{}>","b".repeat(15)),format!("sgml:a{}","b".repeat(15))),(format!("[A{}]","B".repeat(15)),format!("bracket:A{}","B".repeat(15))),("[AB]".into(),"bracket:AB".into()),("<|a_1b_2|>".into(),"ctrl:a_1b_2".into())];
    for (token,id) in names { positive(&format!("{PROSE} "),&token,4," ",&id,"normal"); }
}
#[test] fn upstream_grammar_rejections() {
    for token in [format!("<|a{}|>","b".repeat(32)),format!("<a{}>","b".repeat(16)),format!("[A{}]","B".repeat(16)),"[A]".into(),"<|1abc|>".into(),"<|ab-cd|>".into(),"<ab.cd>".into(),"[Sep]".into(),"<||>".into(),"<>".into(),"</>".into(),"[]".into(),"< s>".into()] { negative(&format!("{PROSE} {}",repeated(&token,5," "))); }
    negative(&repeated("<|Sep|> <|sEp|>",4," "));
    negative(&repeated("<s> </s>",4," "));
    negative("<|sep");
}
#[test] fn upstream_wide_scalar_offsets() { positive("\u{1f30a} ","<|sep|>",4," ","ctrl:sep","normal"); }
#[test] fn upstream_quotation_and_prose_hard_negatives() {
    for text in ["The separator token is <|sep|>.".into(),"<|im_start|> <|im_end|> <s> [PAD]".into(),format!("{PROSE}\n\n{PROSE} {}",repeated("<|sep|>",3," ")),format!("```\nExample configuration:\n{}\n```\n",repeated("<|sep|>",7,"\n")),format!("use `{}` here",repeated("<|im_start|>",3," ")),format!("tokenizer example sequence is {}",repeated("<|eot_id|>",4," ")),format!("{PROSE} {}",repeated("<|sep|>",5,", ")),repeated("<|im_start|> <|im_end|>",10," "),format!("```\ndocumentation:\n{}\n```\n",repeated("<div>",7,"\n")),format!("```\n{}\n```\n",repeated("<div><span><code>",9,"\n")),format!("{PROSE} {}",repeated("[sep]",10," ")),format!("{PROSE} <|unclosed_name and then the prose continues"),format!("    tokenizer fixture:\n    {}",repeated("[SEP]",7," "))] { negative(&text); }
}
#[test] fn upstream_custom_tokens_and_overlong_candidate_recovery() {
    for (token,id) in [("<|custom_marker|>","ctrl:custom_marker"),("<div>","sgml:div"),("[MASK]","bracket:MASK")] { positive(&format!("{PROSE} "),token,4," ",id,"normal"); }
    positive(&format!("<|{} {PROSE} ","a".repeat(100)),"<|sep|>",4," ","ctrl:sep","normal");
    matrix(&format!("{PROSE} <|custom_marker|> trailing prose"),|state| { assert!(state.latched.is_none()); let e=state.pending_evidence.as_ref().unwrap(); assert_eq!(e.token_id,"ctrl:custom_marker"); assert!(!e.quotation_like); });
    matrix(&format!("{PROSE} <|sep|> <s> [PAD]"),|state| { assert!(state.latched.is_none()); assert_eq!(state.pending_evidence.as_ref().unwrap().token_id,"bracket:PAD"); });
}
#[test] fn upstream_evidence_zero_gap_and_wrapped_flood() {
    for gap in ["","\n"] { let flood=vec!["!".repeat(80);12].join("\n"); matrix(&format!("<|close|>{gap}{flood}"),|state| { let e=state.pending_evidence.as_ref().unwrap(); assert_eq!(e.start_offset,0); assert_eq!(e.end_offset,9); assert_eq!(e.expires_at_offset,2057); assert_eq!(e.gap_length,gap.len()); assert_eq!(e.first_payload_offset,Some(9+gap.len())); assert!(corroborates_control_leak(e,9+gap.len(),state.current_offset)); assert!(!corroborates_control_leak(e,14+gap.len(),state.current_offset)); }); }
}
#[test] fn upstream_evidence_gap_boundary_and_quotation() {
    for gap in [32,33,40] { matrix(&format!("<|close|>{}{}"," ".repeat(gap),"!".repeat(40)),|state| { let e=state.pending_evidence.as_ref().unwrap(); assert_eq!(corroborates_control_leak(e,9+gap,state.current_offset),gap<=32); }); }
    matrix(&format!("```\n<|close|>\n{}\n```","!".repeat(300)),|state| { let e=state.pending_evidence.as_ref().unwrap(); assert!(e.quotation_like); assert!(corroborates_control_leak(e,14,state.current_offset)); });
}
#[test] fn upstream_evidence_prose_and_latest_family() {
    let prose=" but then the answer continued for a while. ";
    matrix(&format!("<|close|>{prose}{}","!".repeat(300)),|state| { let e=state.pending_evidence.as_ref().unwrap(); assert_eq!(e.first_payload_offset,Some(10)); assert!(!corroborates_control_leak(e,9+prose.len(),state.current_offset)); });
    matrix(&format!("{PROSE} <s> [PAD] tail"),|state| { let e=state.pending_evidence.as_ref().unwrap(); assert_eq!(e.family,TokenFamily::Bracket); assert_eq!(e.token_id,"bracket:PAD"); assert_eq!(e.first_payload_offset,Some(PROSE.len()+11)); });
}
