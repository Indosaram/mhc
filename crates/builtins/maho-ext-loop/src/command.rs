use crate::{parse::LoopTarget,types::{CronEntry,LoopPhase,LoopState}};
pub const LOOP_ARGUMENT_HINT:&str="[interval] [prompt] | stop [id|all] | status | pause | resume";
pub const LOOP_COMMAND_DESCRIPTION:&str="Repeat a prompt on a fixed interval or a self-paced schedule (e.g. /loop 5m check the deploy)";
pub const LOOP_HEADLESS_REJECTION:&str="/loop needs an interactive session; it is not available in print mode, so nothing was armed.";
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct ArgumentCompletion { pub value:String,pub label:String }
pub fn complete_loop_arguments(prefix:&str)->Option<Vec<ArgumentCompletion>> {
    let prefix=prefix.trim_matches(|c|matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).to_lowercase();
    let matches:Vec<_>=["stop","status","pause","resume"].into_iter().filter(|verb|verb.starts_with(&prefix)).map(|verb|ArgumentCompletion { value:verb.into(),label:verb.into() }).collect();
    if matches.is_empty() { None } else { Some(matches) }
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub enum TargetResolution { Apply(LoopTarget),None,Ambiguous(Vec<String>) }
pub fn active_loop_ids(state:&LoopState)->Vec<String> {
    state.entries.values().filter_map(|entry| {
        let (fields,lifecycle)=match entry { CronEntry::Fixed { fields,lifecycle,.. }|CronEntry::Dynamic { fields,lifecycle,.. }=>(fields,lifecycle) };
        (lifecycle.phase!=LoopPhase::Ended).then(||fields.id.clone())
    }).collect()
}
pub fn resolve_command_target(target:&LoopTarget,state:&LoopState)->TargetResolution {
    match target {
        LoopTarget::All|LoopTarget::Id(_)=>TargetResolution::Apply(target.clone()),
        LoopTarget::Implicit=>match active_loop_ids(state).as_slice() {
            []=>TargetResolution::None,
            [id]=>TargetResolution::Apply(LoopTarget::Id(id.clone())),
            ids=>TargetResolution::Ambiguous(ids.to_vec()),
        },
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn completion_trims_js_whitespace_and_filters_prefix() { let result=complete_loop_arguments("\u{feff} ST "); assert_eq!(result.unwrap().into_iter().map(|item|item.value).collect::<Vec<_>>(),["stop","status"]); }
    #[test] fn unmatched_completion_is_absent() { assert_eq!(complete_loop_arguments("other"),None); }
    #[test] fn implicit_target_on_empty_state_applies_nothing() { let state=crate::store::empty_loop_state("s"); assert_eq!(resolve_command_target(&LoopTarget::Implicit,&state),TargetResolution::None); }
    #[test] fn explicit_target_is_not_changed_by_missing_entry() { let state=crate::store::empty_loop_state("s"); assert_eq!(resolve_command_target(&LoopTarget::Id("missing".into()),&state),TargetResolution::Apply(LoopTarget::Id("missing".into()))); }
}
