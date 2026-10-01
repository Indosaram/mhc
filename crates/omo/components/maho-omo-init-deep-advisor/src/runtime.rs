use std::path::{Path,PathBuf};
use maho_ext_api::{CustomMessage,DeliverAs,ExtensionApi,ExtensionContext,ExtensionFailure,ExtensionUiDialogOptions,SendMessageOptions,ToolContent};
use crate::{eligibility::compute_eligibility,git_helpers::{git_head,run},proposed_data::{build_proposed_data,EligibilityResult},state::*};
pub const CHOICES: &[&str]=&["Run now","Skip this time","Never in this project","Never anywhere"];
pub struct AdvisorPreflight { pub root:PathBuf,pub state_dir:PathBuf }
pub async fn run_advisor_after_preflight(api:&ExtensionApi,context:&ExtensionContext,preflight:&AdvisorPreflight,skills_root:&Path,now:f64) -> Result<(),ExtensionFailure> {
    let result=prepare_proposal(preflight,now).map_err(|e|ExtensionFailure::new(e.to_string()))?;
    let Some((repo,eligibility))=result else { return Ok(()); };
    tokio::task::yield_now().await;
    let choices=CHOICES.iter().map(|s|(*s).to_owned()).collect::<Vec<_>>();
    let choice=context.ui.select("Init-deep",&choices,ExtensionUiDialogOptions {timeout_ms:Some(60_000),signal:None}).await;
    handle_choice(choice.as_deref(),api,preflight,&repo,&eligibility,skills_root,now)
}
pub fn prepare_proposal(preflight:&AdvisorPreflight,now:f64) -> std::io::Result<Option<(String,EligibilityResult)>> {
    let repo=repo_hash(&preflight.root)?;
    if is_globally_declined(&preflight.state_dir) || is_project_declined(&preflight.state_dir,&repo) { return Ok(None); }
    let cooldown=read_cooldown_until(&preflight.state_dir,&repo);
    if now<cooldown { return Ok(None); }
    let head=git_head(&preflight.root)?;
    let eligibility=compute_eligibility(&preflight.root,&head,read_last_proposed_head(&preflight.state_dir,&repo).as_deref(),cooldown,now)?;
    let Some(eligibility)=eligibility else { return Ok(None); };
    write_last_proposed_head(&preflight.state_dir,&repo,&head,now)?;
    Ok(Some((repo,eligibility)))
}
pub fn handle_choice(choice:Option<&str>,api:&ExtensionApi,preflight:&AdvisorPreflight,repo:&str,eligibility:&EligibilityResult,skills_root:&Path,now:f64) -> Result<(),ExtensionFailure> {
    let AdvisorPreflight {root,state_dir}=preflight;
    let write=match choice {
        Some("Run now")=> {
            api.send_message(CustomMessage {custom_type:"omo-init-deep-advisor:run".into(),content:vec![ToolContent::text(format!("Read the init-deep skill at {}/init-deep/SKILL.md with the read tool and follow it.",skills_root.display()))],display:false,details:None},SendMessageOptions {trigger_turn:true,deliver_as:Some(DeliverAs::FollowUp)})?;
            let mode=if run(root,&["ls-files","--error-unmatch","AGENTS.md"]).is_ok() {SuggestedMode::Committed} else {SuggestedMode::Local};
            let data=build_proposed_data(repo,eligibility,mode).map_err(|e|ExtensionFailure::new(e.to_string()))?;
            api.append_entry("omo-init-deep-advisor:proposed",Some(data))?;
            return Ok(());
        },
        Some("Skip this time")|None=>write_cooldown(state_dir,repo,now),
        Some("Never in this project")=>write_project_decline(state_dir,repo,now),
        Some("Never anywhere")=>write_global_decline(state_dir,now),
        Some(_)=>return Ok(()),
    };
    write.map_err(|e|ExtensionFailure::new(e.to_string()))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_helpers::tests::repo;
    use std::fs;
    fn preflight() -> (tempfile::TempDir,tempfile::TempDir,AdvisorPreflight) { let (root,_)=repo(); for n in 0..8 { fs::write(root.path().join("src").join(format!("file{n}.ts")),"x").unwrap(); } let state=tempfile::tempdir().unwrap(); let p=AdvisorPreflight {root:root.path().into(),state_dir:state.path().into()}; (root,state,p) }
    #[test] fn proposal_record_precedes_ui() { let (_r,_s,p)=preflight(); let (repo,_)=prepare_proposal(&p,100.0).unwrap().unwrap(); assert_eq!(read_last_proposed_head(&p.state_dir,&repo).unwrap(),git_head(&p.root).unwrap()); assert!(prepare_proposal(&p,100.0).unwrap().is_none()); }
    #[test] fn global_decline_suppresses() { let (_r,_s,p)=preflight(); write_global_decline(&p.state_dir,0.0).unwrap(); assert!(prepare_proposal(&p,100.0).unwrap().is_none()); }
    #[test] fn project_decline_suppresses() { let (_r,_s,p)=preflight(); write_project_decline(&p.state_dir,&repo_hash(&p.root).unwrap(),0.0).unwrap(); assert!(prepare_proposal(&p,100.0).unwrap().is_none()); }
    #[test] fn cooldown_suppresses() { let (_r,_s,p)=preflight(); write_cooldown(&p.state_dir,&repo_hash(&p.root).unwrap(),100.0).unwrap(); assert!(prepare_proposal(&p,101.0).unwrap().is_none()); }
    fn api()->ExtensionApi {use maho_ext_api::{LoadedExtension,SourceInfo,ExtensionSessionProfile,EventBus,ExtensionRuntime};ExtensionApi::new(LoadedExtension::new("advisor",PathBuf::from("/workspace"),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default())}
    fn eligibility()->EligibilityResult {EligibilityResult::SnapshotInvalid {drift:crate::proposed_data::StaleData {stale:true}}}
    #[test] fn skip_writes_cooldown() {let (_r,_s,p)=preflight();let repo=repo_hash(&p.root).unwrap();handle_choice(Some("Skip this time"),&api(),&p,&repo,&eligibility(),Path::new("/skills"),100.0).unwrap();assert!(read_cooldown_until(&p.state_dir,&repo)>100.0);}
    #[test] fn timeout_writes_cooldown() {let (_r,_s,p)=preflight();let repo=repo_hash(&p.root).unwrap();handle_choice(None,&api(),&p,&repo,&eligibility(),Path::new("/skills"),100.0).unwrap();assert!(read_cooldown_until(&p.state_dir,&repo)>100.0);}
    #[test] fn never_project_writes_decline() {let (_r,_s,p)=preflight();let repo=repo_hash(&p.root).unwrap();handle_choice(Some("Never in this project"),&api(),&p,&repo,&eligibility(),Path::new("/skills"),100.0).unwrap();assert!(is_project_declined(&p.state_dir,&repo));assert!(!is_globally_declined(&p.state_dir));}
    #[test] fn never_anywhere_writes_decline() {let (_r,_s,p)=preflight();let repo=repo_hash(&p.root).unwrap();handle_choice(Some("Never anywhere"),&api(),&p,&repo,&eligibility(),Path::new("/skills"),100.0).unwrap();assert!(is_globally_declined(&p.state_dir));}
    #[derive(Default)]
    struct Actions {messages:std::sync::Mutex<Vec<(CustomMessage,SendMessageOptions)>>,entries:std::sync::Mutex<Vec<(String,Option<serde_json::Value>)>>}
    impl maho_ext_api::ExtensionActions for Actions {
        fn send_message(&self,m:CustomMessage,o:SendMessageOptions)->Result<(),ExtensionFailure> {self.messages.lock().unwrap().push((m,o));Ok(())}
        fn send_user_message(&self,_:maho_ext_api::UserMessageContent,_:maho_ext_api::SendUserMessageOptions)->Result<(),ExtensionFailure> {Err(ExtensionFailure::new("unexpected user message"))}
        fn append_entry(&self,t:&str,v:Option<serde_json::Value>)->Result<(),ExtensionFailure> {self.entries.lock().unwrap().push((t.into(),v));Ok(())}
        fn get_all_tools(&self)->Result<Vec<maho_ext_api::ToolInfo>,ExtensionFailure> {Ok(Vec::new())}
    }
    #[test] fn run_now_hidden_followup_and_local_entry() {let (_r,_s,p)=preflight();let a=api();let actions=std::sync::Arc::new(Actions::default());a.runtime.bind(actions.clone());handle_choice(Some("Run now"),&a,&p,"repo",&eligibility(),Path::new("/skills"),100.0).unwrap();let messages=actions.messages.lock().unwrap();assert_eq!(messages.len(),1);assert!(!messages[0].0.display);assert_eq!(messages[0].0.custom_type,"omo-init-deep-advisor:run");assert!(messages[0].1.trigger_turn);assert_eq!(messages[0].1.deliver_as,Some(DeliverAs::FollowUp));let entries=actions.entries.lock().unwrap();assert_eq!(entries[0].0,"omo-init-deep-advisor:proposed");assert_eq!(entries[0].1.as_ref().unwrap()["suggestedMode"],"local");}
    #[test] fn tracked_agents_suggests_committed() {let (_r,_s,p)=preflight();fs::write(p.root.join("AGENTS.md"),"").unwrap();run(&p.root,&["add","AGENTS.md"]).unwrap();let a=api();let actions=std::sync::Arc::new(Actions::default());a.runtime.bind(actions.clone());handle_choice(Some("Run now"),&a,&p,"repo",&eligibility(),Path::new("/skills"),100.0).unwrap();assert_eq!(actions.entries.lock().unwrap()[0].1.as_ref().unwrap()["suggestedMode"],"committed");}
}
