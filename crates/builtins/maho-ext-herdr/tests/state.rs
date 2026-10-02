use maho_ext_herdr::state::*;
use serde_json::json;
#[test]
fn idle_working_blocked(){let mut state=HerdrState::default();assert_eq!(select_herdr_report(&state).state,"idle");state=reduce_herdr_state(state,HerdrStateEvent::Turn{active:true});assert_eq!(select_herdr_report(&state).state,"working");state=reduce_herdr_state(state,HerdrStateEvent::Blocked{active:true,id:"q".into(),label:Some("question".into())});assert_eq!(select_herdr_report(&state).message.as_deref(),Some("question"));state=reduce_herdr_state(state,HerdrStateEvent::Blocked{active:false,id:"q".into(),label:None});assert_eq!(select_herdr_report(&state).state,"working");}
#[test]
fn insertion_order_and_duplicate(){let mut state=HerdrState::default();for (id,label) in [("z","first"),("a","second"),("z","replacement")]{state=reduce_herdr_state(state,HerdrStateEvent::Blocked{active:true,id:id.into(),label:Some(label.into())});}assert_eq!(state.blocked.len(),2);assert_eq!(select_herdr_report(&state).message.as_deref(),Some("first"));}
#[test]
fn children_and_monitors(){let state=reduce_herdr_state(reduce_herdr_state(HerdrState::default(),HerdrStateEvent::Children{count:1}),HerdrStateEvent::Monitors{count:2});assert_eq!(select_herdr_report(&state).message.as_deref(),Some("1 subagent running + 2 monitors live"));}
#[test]
fn blocked_validation(){assert!(is_herdr_blocked_event(&json!({"active":true,"id":"q"})));for value in [json!(null),json!({"active":true,"id":""}),json!({"active":1,"id":"q"}),json!({"active":true,"id":"q","label":null})]{assert!(!is_herdr_blocked_event(&value));}}
