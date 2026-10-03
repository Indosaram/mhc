use serde_json::Value;
use crate::todo_types::*;
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct TodoNormalization { pub entry:Option<TodoOpEntry>,pub corrections:Vec<String>,pub error:Option<String> }
const CONFLICT:&str="conflicting shapes. Use {\"op\":\"init\",\"list\":[...]} to replace the list or {\"op\":\"append\",\"phase\":\"<active phase>\",\"items\":[...]} to add tasks.";
fn error(message:&str)->TodoNormalization { TodoNormalization{entry:None,corrections:vec![],error:Some(message.into())} }
fn strings(value:Option<&Value>)->Option<Vec<String>> { value?.as_array()?.iter().map(|v|v.as_str().map(String::from)).collect() }
fn list(value:Option<&Value>)->Option<Vec<TodoPhaseInput>> { value?.as_array()?.iter().map(|v|Some(TodoPhaseInput{phase:v.get("phase")?.as_str()?.into(),items:strings(v.get("items"))?})).collect() }
fn js_whitespace(c:char)->bool { matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }
fn non_blank(value:Option<&Value>)->Option<String> { value?.as_str().filter(|s|!s.trim_matches(js_whitespace).is_empty()).map(String::from) }
fn blank(value:Option<&Value>)->bool { value.and_then(Value::as_str).is_some_and(|s|s.trim_matches(js_whitespace).is_empty()) }
fn op_name(op:TodoOperation)-> &'static str { match op { TodoOperation::Init=>"init",TodoOperation::Start=>"start",TodoOperation::Done=>"done",TodoOperation::Rm=>"rm",TodoOperation::Drop=>"drop",TodoOperation::Append=>"append",TodoOperation::View=>"view" } }
fn inferred(op:&str,reason:&str,form:&str)->String { format!("[auto-corrected] \"op\" was missing; interpreted as \"{op}\" because {reason}. Always pass op explicitly: {form}") }
pub fn normalize_todo_params(raw:&Value,current:&[TodoPhase])->TodoNormalization {
    let raw_op=raw.get("op");
    let op=match raw_op.and_then(Value::as_str) {
        Some("view")=>return TodoNormalization{entry:Some(TodoOpEntry{op:TodoOperation::View,list:None,task:None,phase:None,items:None}),corrections:vec![],error:None},
        Some("init")=>Some(TodoOperation::Init),Some("start")=>Some(TodoOperation::Start),Some("done")=>Some(TodoOperation::Done),Some("rm")=>Some(TodoOperation::Rm),Some("drop")=>Some(TodoOperation::Drop),Some("append")=>Some(TodoOperation::Append),
        _ if raw_op.is_some()=>return error("Invalid \"op\"."),_=>None,
    };
    let task=non_blank(raw.get("task")); let phase=non_blank(raw.get("phase")); let mut corrections=vec![];
    if matches!(op,Some(TodoOperation::Start|TodoOperation::Done|TodoOperation::Drop|TodoOperation::Rm)) {
        if op==Some(TodoOperation::Rm) && blank(raw.get("task")) && blank(raw.get("phase")) {
            corrections.push("[auto-corrected] \"task\" and \"phase\" were both blank; treated as a bulk clear. For a single target pass the exact text: {\"op\":\"rm\",\"task\":\"<exact task content>\"}.".into());
        } else if blank(raw.get("task")) && phase.is_none() { return error("Blank \"task\" — pass the exact task text, or omit the field entirely for a bulk operation."); }
        else if blank(raw.get("phase")) && task.is_none() { return error("Blank \"phase\" — pass the exact phase text, or omit the field entirely for a bulk operation."); }
    }
    let mut items=strings(raw.get("items")).filter(|v|!v.is_empty());
    let aliases:Vec<_>=[TodoOperation::Init,TodoOperation::Append].into_iter().filter_map(|alias|strings(raw.get(op_name(alias))).filter(|v|!v.is_empty()).map(|items|(alias,items))).collect();
    if aliases.len()>1 || (!aliases.is_empty() && items.is_some()) { return error(CONFLICT); }
    let mut candidate=None;
    if let Some((alias,alias_items))=aliases.into_iter().next() {
        if op.is_some_and(|op|op!=alias) { return error(CONFLICT); }
        items=Some(alias_items); if op.is_none() { candidate=Some(alias); }
        let name=op_name(alias); let form=if alias==TodoOperation::Init { "{\"op\":\"init\",\"items\":[...]}" } else { "{\"op\":\"append\",\"phase\":\"<active phase>\",\"items\":[...]}" };
        corrections.push(if op.is_none() { inferred(name,&format!("\"{name}\" was provided as an items alias"),form) } else { format!("[auto-corrected] \"{name}\" was used as an items alias and folded into \"items\". Use: {form}") });
    }
    let list=list(raw.get("list")); let has_list=list.as_ref().is_some_and(|v|!v.is_empty()); let has_items=items.is_some();
    if has_list && has_items { return error(CONFLICT); }
    let effective=if let Some(op)=op.or(candidate) { op }
        else if has_list { corrections.push(inferred("init","\"list\" was provided","{\"op\":\"init\",\"list\":[...]}")); TodoOperation::Init }
        else if has_items && phase.is_some() { corrections.push(inferred("append","\"items\" and \"phase\" were provided","{\"op\":\"append\",\"phase\":\"<active phase>\",\"items\":[...]}")); TodoOperation::Append }
        else if has_items && current.iter().all(|p|p.tasks.is_empty()) { corrections.push(inferred("init","\"items\" was provided to an empty todo list","{\"op\":\"init\",\"items\":[...]}")); TodoOperation::Init }
        else if has_items { return error("Missing \"op\": use {\"op\":\"init\",\"list\":[...]} to replace the list, or {\"op\":\"append\",\"phase\":\"<active phase>\",\"items\":[...]} to add tasks."); }
        else { return error("Missing \"op\". Example: {\"op\":\"init\",\"list\":[{\"phase\":\"Setup\",\"items\":[\"...\"]}]}"); };
    let mut entry=TodoOpEntry{op:effective,list:None,task:None,phase:None,items:None};
    match effective {
        TodoOperation::Init => { if task.is_some() || (list.as_ref().is_some_and(Vec::is_empty) && has_items) { return error(CONFLICT); } entry.list=list; entry.items=items; entry.phase=phase; }
        TodoOperation::Start => { if phase.is_some() || has_list || has_items { return error(CONFLICT); } entry.task=task; }
        TodoOperation::Done|TodoOperation::Drop|TodoOperation::Rm => { if has_list || has_items { return error(CONFLICT); } entry.task=task; entry.phase=phase; }
        TodoOperation::Append => { if task.is_some() || has_list { return error(CONFLICT); } entry.phase=phase; entry.items=items; }
        TodoOperation::View => {},
    }
    TodoNormalization{entry:Some(entry),corrections,error:None}
}
#[cfg(test)]
mod tests {
    use super::*; use serde_json::json;
    #[test] fn incoherent_fields_are_rejected_for_every_mutating_operation() { for raw in [json!({"op":"init","task":"x"}),json!({"op":"start","phase":"Tasks"}),json!({"op":"start","list":[{"phase":"A","items":["x"]}]}),json!({"op":"drop","items":["x"]}),json!({"op":"rm","list":[{"phase":"A","items":["x"]}]}),json!({"op":"append","list":[{"phase":"A","items":["x"]}]}),json!({"op":"append","task":"x","items":["y"]})] { assert!(normalize_todo_params(&raw,&[]).entry.is_none()); } }
    #[test] fn alias_append_omits_blank_phase() { let result=normalize_todo_params(&json!({"op":"append","append":["x"],"phase":" \n "}),&[]); assert_eq!(result.entry.unwrap().phase,None); assert_eq!(result.corrections.len(),1); }
    #[test] fn done_preserves_both_targets_for_task_precedence() { let entry=normalize_todo_params(&json!({"op":"done","task":"Existing task","phase":"Tasks"}),&[]).entry.unwrap(); assert_eq!(entry.task,Some("Existing task".into())); assert_eq!(entry.phase,Some("Tasks".into())); }
    #[test] fn padded_clear_does_not_mutate_input() { let raw=json!({"op":"rm","list":[],"task":"","phase":"","items":[]}); let before=raw.clone(); let result=normalize_todo_params(&raw,&[]); assert_eq!(result.entry.unwrap().op,TodoOperation::Rm); assert_eq!(result.corrections.len(),1); assert_eq!(raw,before); }
    #[test] fn both_blank_targets_remain_invalid_for_status_mutations() { for op in ["start","done","drop"] { assert!(normalize_todo_params(&json!({"op":op,"task":"","phase":""}),&[]).entry.is_none()); } }
    #[test] fn padded_done_preserves_task_and_input() { let raw=json!({"op":"done","list":[],"task":"Existing task","phase":"","items":[]}); let saved=raw.clone(); let normalized=normalize_todo_params(&raw,&[]); assert_eq!(normalized.entry.unwrap().task,Some("Existing task".into())); assert!(normalized.corrections.is_empty()); assert_eq!(raw,saved); }
    #[test] fn unrelated_empty_placeholders_are_ignored_on_start() { let normalized=normalize_todo_params(&json!({"op":"start","task":"Existing task","list":[],"items":[],"phase":""}),&[]); let entry=normalized.entry.unwrap(); assert_eq!(entry.task,Some("Existing task".into())); assert_eq!(entry.list,None); assert_eq!(entry.items,None); }
    #[test] fn list_plus_alias_conflicts_before_inference() { assert!(normalize_todo_params(&json!({"list":[{"phase":"A","items":["x"]}],"append":["y"]}),&[]).entry.is_none()); }
    #[test] fn duplicate_phases_are_left_to_transaction_layer() { let raw=json!({"op":"init","list":[{"phase":"A","items":["x"],"items2":[]},{"phase":"A","items":["y"]}]}); let entry=normalize_todo_params(&raw,&[]).entry.unwrap(); assert_eq!(entry.list.unwrap(),vec![TodoPhaseInput{phase:"A".into(),items:vec!["x".into()]},TodoPhaseInput{phase:"A".into(),items:vec!["y".into()]}]); }
    #[test] fn blank_targets_use_javascript_whitespace() { assert!(normalize_todo_params(&json!({"op":"done","task":"\u{feff}"}),&[]).error.is_some()); assert_eq!(normalize_todo_params(&json!({"op":"done","task":"\u{0085}"}),&[]).entry.unwrap().task,Some("\u{0085}".into())); }
    #[test] fn rejects_each_single_blank_target() { for op in ["start","done","drop","rm"] { for target in ["task","phase"] { let raw=json!({"op":op,target:" \t "}); assert!(normalize_todo_params(&raw,&[]).error.is_some()); } } }
    #[test] fn padded_rm_is_explicit_clear() { let result=normalize_todo_params(&json!({"op":"rm","task":"","phase":""}),&[]); assert_eq!(result.entry.unwrap().op,TodoOperation::Rm); assert_eq!(result.corrections.len(),1); }
    #[test] fn real_phase_drops_blank_task() { let result=normalize_todo_params(&json!({"op":"done","task":" ","phase":"Tasks"}),&[]); assert_eq!(result.entry.unwrap().phase,Some("Tasks".into())); }
    #[test] fn explicit_empty_init_list_is_preserved() { assert_eq!(normalize_todo_params(&json!({"op":"init","list":[]}),&[]).entry.unwrap().list,Some(vec![])); }
    #[test] fn view_ignores_every_other_field() { let result=normalize_todo_params(&json!({"op":"view","task":" ","list":[{"bad":true}],"append":["x"]}),&[]); assert_eq!(result.entry.unwrap().op,TodoOperation::View); assert!(result.corrections.is_empty()); }
    #[test] fn alias_infers_init() { let result=normalize_todo_params(&json!({"init":["First"]}),&[]); assert_eq!(result.entry.unwrap().items,Some(vec!["First".into()])); assert_eq!(result.corrections.len(),1); }
    #[test] fn explicit_alias_folds_items() { assert_eq!(normalize_todo_params(&json!({"op":"append","append":["x"]}),&[]).entry.unwrap().items,Some(vec!["x".into()])); }
    #[test] fn conflicting_aliases_fail() { for raw in [json!({"init":["x"],"append":["y"]}),json!({"op":"init","append":["x"]}),json!({"append":["x"],"items":["y"]})] { assert!(normalize_todo_params(&raw,&[]).error.is_some()); } }
    #[test] fn list_infers_init() { assert_eq!(normalize_todo_params(&json!({"list":[{"phase":"A","items":["x"]}]}),&[]).entry.unwrap().op,TodoOperation::Init); }
    #[test] fn items_on_empty_list_infer_init() { assert_eq!(normalize_todo_params(&json!({"items":["x"]}),&[]).entry.unwrap().op,TodoOperation::Init); }
    #[test] fn items_and_phase_infer_append() { assert_eq!(normalize_todo_params(&json!({"items":["x"],"phase":"Later"}),&[]).entry.unwrap().op,TodoOperation::Append); }
    #[test] fn populated_items_need_operation() { let current=vec![TodoPhase{name:"Tasks".into(),tasks:vec![TodoItem{content:"old".into(),status:TodoStatus::Pending}]}]; assert!(normalize_todo_params(&json!({"items":["x"]}),&current).error.is_some()); }
    #[test] fn list_items_conflict() { assert!(normalize_todo_params(&json!({"list":[{"phase":"A","items":["x"]}],"items":["y"]}),&[]).error.is_some()); }
    #[test] fn empty_signals_do_not_infer_operation() { assert!(normalize_todo_params(&json!({"list":[],"items":[],"phase":" "}),&[]).error.is_some()); }
    #[test] fn invalid_operation_fails() { assert!(normalize_todo_params(&json!({"op":"invalid"}),&[]).error.is_some()); }
    #[test] fn done_list_cannot_bulk_complete() { assert!(normalize_todo_params(&json!({"op":"done","list":[{"phase":"A","items":["x"]}]}),&[]).error.is_some()); }
    #[test] fn explicit_clear_and_items_conflict() { assert!(normalize_todo_params(&json!({"op":"init","list":[],"items":["x"]}),&[]).error.is_some()); }
    #[test] fn upstream_whitespace_padded_rm_has_no_targets() {
        let result=normalize_todo_params(&json!({"op":"rm","task":" \t","phase":" \n "}),&[]);
        assert!(result.error.is_none());assert_eq!(result.corrections.len(),1);assert_eq!(result.entry,Some(TodoOpEntry{op:TodoOperation::Rm,list:None,task:None,phase:None,items:None}));
    }
}
