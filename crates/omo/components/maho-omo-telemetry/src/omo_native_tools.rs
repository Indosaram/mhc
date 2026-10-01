use std::{collections::{HashMap,HashSet},path::Path};
use serde_json::{Value,json};
use crate::product_identity::BUILTIN_SKILL_NAMES;
pub fn matches_tool_name(name:&str,expected:&str)->bool {let name=name.trim().to_lowercase().replace('-',"_");let expected=expected.trim().to_lowercase().replace('-',"_");name==expected || ["_",":","/"].iter().any(|s|name.ends_with(&format!("{s}{expected}")))}
pub fn builtin_skill_name(root:&Path,target:&Path)->Option<String> {let root=root.canonicalize().ok()?;let target=target.canonicalize().ok()?;let relative=target.strip_prefix(root).ok()?;let parts:Vec<_>=relative.components().collect();if parts.len()!=2 || parts[1].as_os_str()!="SKILL.md" {return None;}let name=parts[0].as_os_str().to_str()?;BUILTIN_SKILL_NAMES.contains(&name).then(||name.into())}
fn identifier(v:Option<&Value>)->Option<&str> {v?.as_str().map(str::trim).filter(|s|!s.is_empty())}
pub fn builtin_skill_name_from_windows_canonical_paths(root:&str,target:&str)->Option<String> {
    let normalize=|path:&str| {let path=if let Some(rest)=path.strip_prefix("\\\\?\\UNC\\") {format!("\\\\{rest}")} else {path.strip_prefix("\\\\?\\").unwrap_or(path).into()};path.to_lowercase()};
    let root=normalize(root);let target=normalize(target);let prefix=format!("{}\\",root.trim_end_matches('\\'));let relative=target.strip_prefix(&prefix)?;let parts:Vec<_>=relative.split('\\').collect();if parts.len()!=2 || parts[1]!="skill.md" {return None;}BUILTIN_SKILL_NAMES.contains(&parts[0]).then(||parts[0].into())
}
fn target(item:&Value,parent:&Value)->Option<(&'static str,String)> {let category=identifier(item.get("category"));let agent=identifier(item.get("subagent_type"));let category=category.or_else(||if agent.is_none() {identifier(parent.get("category"))} else {None});let agent=agent.or_else(||if identifier(item.get("category")).is_none() {identifier(parent.get("subagent_type"))} else {None});match (category,agent) {(Some(c),None)=>Some(("category",c.into())),(None,Some(a))=>Some(("subagent",a.into())),_=>None}}
#[derive(Default)]
pub struct ToolTelemetry {feature_usage:HashMap<String,HashSet<&'static str>>}
impl ToolTelemetry {
    pub fn clear(&mut self,session:&str) {self.feature_usage.remove(session);}
    pub fn record(&mut self,event:&Value,session:&str,hash:&str,cwd:&Path,skills_root:&Path)->Vec<(&'static str,Value)> {
        let Some(name)=event.get("toolName").and_then(Value::as_str) else {return Vec::new();};
        if event.get("type").and_then(Value::as_str)!=Some("tool_result") || !event.get("input").is_some_and(Value::is_object) || session.is_empty() {return Vec::new();}
        let input=&event["input"];let error=event.get("isError").and_then(Value::as_bool)==Some(true);
        if matches_tool_name(name,"read") && !error {return input.get("path").and_then(Value::as_str).and_then(|path|builtin_skill_name(skills_root,&cwd.join(path))).map(|skill|vec![("skill_loaded",json!({"$session_id":hash,"skill_name":skill}))]).unwrap_or_default();}
        if matches_tool_name(name,"task") && !error {
            let batch=input.get("tasks").and_then(Value::as_array);let size=batch.map_or(1,Vec::len);let targets:Vec<_>=match batch {Some(items)=>items.iter().filter(|v|v.is_object()).filter_map(|v|target(v,input)).collect(),None=>target(input,input).into_iter().collect()};
            return targets.into_iter().map(|(kind,name)| {let known=if kind=="category" {senpi_task::category::BUILTIN_CATEGORY_DEFAULTS.iter().any(|c|c.name==name)} else {senpi_task::agents::curated_readonly_agent_names().contains(name.as_str())};("delegation_started",json!({"$session_id":hash,"kind":kind,"name":if known {name.as_str()} else {"custom"},"background":input.get("run_in_background").and_then(Value::as_bool)==Some(true),"batch_size_bucket":if size<=1 {"1"} else if size<=4 {"2_4"} else {"5_plus"}}))}).collect();
        }
        let feature=if matches_tool_name(name,"create_goal") {Some("goal_tool")} else if matches_tool_name(name,"team_create") {Some("team_create")} else if matches_tool_name(name,"memory_apply_patch") || matches_tool_name(name,"memory") {Some("memory_tool")} else {None};
        if let Some(feature)=feature && self.feature_usage.entry(session.into()).or_default().insert(feature) {vec![("feature_used",json!({"$session_id":hash,"feature":feature}))]} else {Vec::new()}
    }
}
pub fn register_omo_native_tool_telemetry(api:&mut maho_ext_api::ExtensionApi,skills_root:std::path::PathBuf,hash:std::sync::Arc<dyn Fn(&str)->String+Send+Sync>,capture:crate::omo_native_parallel_summary::SummaryCapture) {
    use std::sync::{Arc,Mutex};
    use maho_ext_api::{EventKind,EventResult,ExtensionEvent};
    let state=Arc::new(Mutex::new(ToolTelemetry::default()));let results=Arc::clone(&state);
    api.on(EventKind::ToolResult,Arc::new(move |event,ctx| {let at_session=ctx.session_manager.session_id();let output=if let ExtensionEvent::ToolResult(event)=event {results.lock().unwrap_or_else(std::sync::PoisonError::into_inner).record(&json!({"type":"tool_result","toolName":event.tool_name,"input":event.input,"isError":event.is_error}),at_session,&hash(at_session),&ctx.cwd,&skills_root)} else {Vec::new()};let capture=Arc::clone(&capture);Box::pin(async move {for (name,properties) in output {capture(name,properties);}Ok(EventResult::None)})}));
    api.on(EventKind::SessionShutdown,Arc::new(move |_,ctx| {state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear(ctx.session_manager.session_id());Box::pin(async {Ok(EventResult::None)})}));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn windows_case_and_namespaces() {let root=r"C:\Users\runneradmin\plugin\skills";assert_eq!(builtin_skill_name_from_windows_canonical_paths(root,r"c:\users\RUNNERADMIN\plugin\SKILLS\DEBUGGING\skill.md"),Some("debugging".into()));assert_eq!(builtin_skill_name_from_windows_canonical_paths(root,r"\\?\C:\Users\runneradmin\plugin\skills\ulw-plan\SKILL.md"),Some("ulw-plan".into()));assert_eq!(builtin_skill_name_from_windows_canonical_paths(r"\\server\share\plugin\skills",r"\\?\UNC\server\share\plugin\skills\debugging\SKILL.md"),Some("debugging".into()));}
    #[test] fn windows_escape_rejected() {let root=r"C:\Users\runneradmin\plugin\skills";for path in [r"C:\Users\runneradmin\plugin\notes\debugging\SKILL.md",r"C:\Users\runneradmin\plugin\skills-escape\debugging\SKILL.md",r"D:\plugin\skills\debugging\SKILL.md",r"\\?\UNC\server\share\plugin\notes\debugging\SKILL.md"] {assert!(builtin_skill_name_from_windows_canonical_paths(root,path).is_none());}}
    #[test] fn aliases() {for name in ["read","functions.read","tool_read"," TOOL-READ ","tool:read","tool/read"] {assert_eq!(matches_tool_name(name,"read"),name!="functions.read");}}
    #[test] fn feature_once_and_reset() {let mut t=ToolTelemetry::default();let e=json!({"type":"tool_result","toolName":"create_goal","input":{},"isError":true});assert_eq!(t.record(&e,"s","h",Path::new("/"),Path::new("/")).len(),1);assert!(t.record(&e,"s","h",Path::new("/"),Path::new("/")).is_empty());t.clear("s");assert_eq!(t.record(&e,"s","h",Path::new("/"),Path::new("/")).len(),1);}
    #[test] fn child_target_overrides_parent_kind() {let e=json!({"category":"quick","tasks":[{"subagent_type":"custom-agent"},{}]});let items=e["tasks"].as_array().unwrap();assert_eq!(target(&items[0],&e),Some(("subagent","custom-agent".into())));assert_eq!(target(&items[1],&e),Some(("category","quick".into())));}
    #[test] fn ambiguous_target_rejected() {assert!(target(&json!({"category":"quick","subagent_type":"agent"}),&json!({})).is_none());}
    #[test] fn skills_must_be_inside_root() {let t=tempfile::tempdir().unwrap();let root=t.path().join("skills");std::fs::create_dir_all(root.join("programming")).unwrap();let p=root.join("programming/SKILL.md");std::fs::write(&p,"").unwrap();assert_eq!(builtin_skill_name(&root,&p),Some("programming".into()));assert!(builtin_skill_name(&root,t.path()).is_none());}
    #[test] fn batch_shared_bucket() {let mut t=ToolTelemetry::default();let e=json!({"type":"tool_result","toolName":"omo/task","input":{"category":"deep","run_in_background":true,"tasks":[{},{},{}]}});let events=t.record(&e,"s","hash",Path::new("/"),Path::new("/"));assert_eq!(events.len(),3);for (_,p) in events {assert_eq!(p,json!({"$session_id":"hash","kind":"category","name":"deep","background":true,"batch_size_bucket":"2_4"}));}}
    #[test] fn unknown_delegation_masked() {let mut t=ToolTelemetry::default();for (input,expected) in [(json!({"subagent_type":"explore"}),"explore"),(json!({"category":"private-category"}),"custom")] {let e=json!({"type":"tool_result","toolName":"task","input":input});let events=t.record(&e,"s","hash",Path::new("/"),Path::new("/"));assert_eq!(events[0].1["name"],expected);}}
    #[test] fn missing_input_ignored() {let mut t=ToolTelemetry::default();assert!(t.record(&json!({"type":"tool_result","toolName":"read"}),"s","hash",Path::new("/"),Path::new("/")).is_empty());}
    #[test] fn exact_event_allowlists() {let mut t=ToolTelemetry::default();for name in ["task","create_goal"] {let e=json!({"type":"tool_result","toolName":name,"input":{"subagent_type":"explore"}});for (kind,p) in t.record(&e,"s","hash",Path::new("/"),Path::new("/")) {let (_,expected)=crate::product_identity::EVENT_PROPERTY_ALLOWLISTS.iter().find(|(event,_)|*event==kind).unwrap();let mut expected=expected.to_vec();expected.sort_unstable();let mut keys:Vec<_>=p.as_object().unwrap().keys().map(String::as_str).collect();keys.sort_unstable();assert_eq!(keys,expected);}}}
    #[test] fn symlink_escape_rejected() {let t=tempfile::tempdir().unwrap();let root=t.path().join("skills");let outside=t.path().join("outside");std::fs::create_dir(&root).unwrap();std::fs::create_dir(&outside).unwrap();std::fs::write(outside.join("SKILL.md"),"").unwrap();std::os::unix::fs::symlink(&outside,root.join("debugging")).unwrap();assert!(builtin_skill_name(&root,&root.join("debugging/SKILL.md")).is_none());}
}
