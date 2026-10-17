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
fn wake(state:HerdrState,source:&str,count:u64)->HerdrState{reduce_herdr_state(state,HerdrStateEvent::WakeSource{source:source.into(),count})}
#[test]
fn wake_source_reduction(){
    let state=wake(HerdrState::default(),"omo-dag",1);assert_eq!(state.wake_sources.get("omo-dag"),Some(&1));
    let unchanged=wake(state.clone(),"omo-dag",1);assert_eq!(unchanged,state);
    let state=wake(state,"omo-dag",0);assert!(state.wake_sources.is_empty());assert_eq!(select_herdr_report(&state),HerdrReport{state:"idle".into(),message:None});
    let state=wake(HerdrState::default(),"never-seen",0);assert!(state.wake_sources.is_empty());
}
#[test]
fn wake_source_labels_after_turn_end(){
    let mut state=HerdrState::default();
    for (source,count) in [("terminal-background-sessions",2),("omo-dag",1),("senpi-codemode",1),("loop-guard-hard-stop",1),("zz-custom",3)]{state=wake(state,source,count);}
    assert_eq!(select_herdr_report(&state),HerdrReport{state:"working".into(),message:Some("1 loop-guard recovery pending + 1 DAG run + 1 detached eval cell + 2 background sessions + 3 zz-custom".into())});
    for (source,count) in [("terminal-background-sessions",1),("omo-dag",2),("senpi-codemode",3),("loop-guard-hard-stop",2),("zz-custom",0)]{state=wake(state,source,count);}
    assert_eq!(select_herdr_report(&state).message.as_deref(),Some("2 loop-guard recoveries pending + 2 DAG runs + 3 detached eval cells + 1 background session"));
}
#[test]
fn represented_wake_sources_are_not_duplicated(){
    let mut state=reduce_herdr_state(HerdrState::default(),HerdrStateEvent::Children{count:1});
    for (source,count) in [("senpi-task",3),("terminal-monitors",2),("ask-user",1)]{state=wake(state,source,count);}
    assert_eq!(select_herdr_report(&state).message.as_deref(),Some("3 subagents running"));
    state=reduce_herdr_state(state,HerdrStateEvent::Children{count:4});
    assert_eq!(select_herdr_report(&state).message.as_deref(),Some("4 subagents running"));
}
