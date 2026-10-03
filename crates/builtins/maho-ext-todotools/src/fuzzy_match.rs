use std::collections::BTreeMap;
use crate::{todo_format::sanitize_todo_text, todo_types::TodoPhase};

#[derive(Clone,Debug,PartialEq,Eq)]
pub struct FuzzyTaskResolution { pub hit:Option<(usize,usize)>, pub corrected:bool, pub suggestion:Option<String> }
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct FuzzyPhaseResolution { pub hit:Option<usize>, pub corrected:bool, pub suggestion:Option<String> }
fn normalized(text:&str)->String { sanitize_todo_text(text).to_lowercase() }
fn dice_coefficient(left:&str,right:&str)->f64 {
    if left == right { return 1.0; }
    let left:Vec<_> = left.encode_utf16().collect();
    let right:Vec<_> = right.encode_utf16().collect();
    if left.len()<2 || right.len()<2 { return 0.0; }
    let mut pairs=BTreeMap::new();
    for pair in left.windows(2) { *pairs.entry((pair[0],pair[1])).or_insert(0u32)+=1; }
    let mut overlap=0u32;
    for pair in right.windows(2) {
        if let Some(count)=pairs.get_mut(&(pair[0],pair[1])) && *count>0 { overlap+=1; *count-=1; }
    }
    2.0*f64::from(overlap)/(left.len()+right.len()-2) as f64
}
fn suggestion_for<'a>(contents:impl Iterator<Item=&'a str>,query:&str)->Option<String> {
    let query=normalized(query);
    if query.is_empty() { return None; }
    let mut suggestion=None;
    let mut best_score=0.0;
    for content in contents {
        let norm=normalized(content);
        if norm.is_empty() { continue; }
        let score=if norm.contains(&query) || query.contains(&norm) { 1.0 } else { dice_coefficient(&norm,&query) };
        if score>=0.5 && score>best_score { best_score=score; suggestion=Some(content.into()); }
    }
    suggestion
}
pub fn fuzzy_resolve_task(phases:&[TodoPhase],query:&str)->FuzzyTaskResolution {
    let candidates:Vec<_>=phases.iter().enumerate().flat_map(|(p,phase)|phase.tasks.iter().enumerate().map(move |(t,task)|((p,t),task.content.as_str()))).collect();
    let exact:Vec<_>=candidates.iter().filter(|(_,content)| *content==query).collect();
    if exact.len()==1 { return FuzzyTaskResolution{hit:Some(exact[0].0),corrected:false,suggestion:None}; }
    let norm=normalized(query);
    let matches:Vec<_>=candidates.iter().filter(|(_,content)|normalized(content)==norm).collect();
    if matches.len()==1 { return FuzzyTaskResolution{hit:Some(matches[0].0),corrected:true,suggestion:None}; }
    FuzzyTaskResolution{hit:None,corrected:false,suggestion:suggestion_for(candidates.iter().map(|(_,content)|*content),query)}
}
pub fn fuzzy_resolve_phase(phases:&[TodoPhase],query:&str)->FuzzyPhaseResolution {
    let exact:Vec<_>=phases.iter().enumerate().filter(|(_,phase)|phase.name==query).collect();
    if exact.len()==1 { return FuzzyPhaseResolution{hit:Some(exact[0].0),corrected:false,suggestion:None}; }
    let norm=normalized(query);
    let matches:Vec<_>=phases.iter().enumerate().filter(|(_,phase)|normalized(&phase.name)==norm).collect();
    if matches.len()==1 { return FuzzyPhaseResolution{hit:Some(matches[0].0),corrected:true,suggestion:None}; }
    FuzzyPhaseResolution{hit:None,corrected:false,suggestion:suggestion_for(phases.iter().map(|p|p.name.as_str()),query)}
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo_types::{TodoItem,TodoStatus};
    fn phases(contents:&[&str])->Vec<TodoPhase> { vec![TodoPhase{name:"Verification".into(),tasks:contents.iter().map(|c|TodoItem{content:(*c).into(),status:TodoStatus::Pending}).collect()}] }
    #[test] fn exact_unique_task_is_not_corrected() { assert_eq!(fuzzy_resolve_task(&phases(&["Deploy API"]),"Deploy API"),FuzzyTaskResolution{hit:Some((0,0)),corrected:false,suggestion:None}); }
    #[test] fn normalized_ansi_task_is_corrected() { assert_eq!(fuzzy_resolve_task(&phases(&["\x1b[31mDeploy   API\x1b[0m"])," deploy api ").hit,Some((0,0))); assert!(fuzzy_resolve_task(&phases(&["Deploy API"]),"DEPLOY API").corrected); }
    #[test] fn normalized_phase_is_corrected() { assert_eq!(fuzzy_resolve_phase(&phases(&[])," verification "),FuzzyPhaseResolution{hit:Some(0),corrected:true,suggestion:None}); }
    #[test] fn duplicate_normalized_tasks_are_only_suggested() { assert_eq!(fuzzy_resolve_task(&phases(&["Deploy API"," deploy   api "]),"DEPLOY API"),FuzzyTaskResolution{hit:None,corrected:false,suggestion:Some("Deploy API".into())}); }
    #[test] fn containment_is_never_a_mutation_target() { assert_eq!(fuzzy_resolve_task(&phases(&["Do not deploy API to production"]),"Deploy API to production").hit,None); }
    #[test] fn paraphrase_dice_is_only_a_suggestion() { assert_eq!(fuzzy_resolve_task(&phases(&["Synthesize non-overlapping findings and current status"]),"Synthesize findings and current status").suggestion,Some("Synthesize non-overlapping findings and current status".into())); }
    #[test] fn empty_query_does_not_suggest() { assert_eq!(fuzzy_resolve_task(&phases(&["Deploy API"])," ").suggestion,None); }
}
