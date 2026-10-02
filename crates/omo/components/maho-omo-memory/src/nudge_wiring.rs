use std::collections::BTreeMap;
use serde::{Deserialize,Serialize};
use serde_json::Value;
use memory_core::git::{GitMemoryRepo,errors::GitError};
use crate::context::MemoryIdentityContext;
pub const ACCEPTED_TURNS_ENTRY_TYPE:&str="omo-memory:accepted-turns";
const MAX_SAFE_INTEGER:u64=9_007_199_254_740_991;
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct AcceptedTurnsRecord{pub version:u32,pub session_id:String,pub prior_user_turns:u64,pub session_baseline_turns:u64}
pub struct ResolvedNudgeSettings{pub enabled:bool,pub every_user_turns:i64}
#[derive(Default)]
pub struct MemoryNudgeWiring{sessions:BTreeMap<String,AcceptedTurnsRecord>,pending_inputs:BTreeMap<String,String>}
fn parse_turn(value:&str)->Option<u64>{if value.is_empty()||!value.bytes().all(|byte|byte.is_ascii_digit()){return None;}value.parse::<u64>().ok().filter(|turn|*turn<=MAX_SAFE_INTEGER)}
impl MemoryNudgeWiring {
    pub fn register(wiring:std::sync::Arc<std::sync::Mutex<Self>>,api:&mut maho_ext_api::ExtensionApi,resolve:crate::prompt::PromptContextResolver) {
        let actions=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory-nudge",api.cwd.clone(),Default::default()),api.profile.clone(),api.events.clone(),api.runtime.clone());
        let actions=std::sync::Arc::new(actions);
        for kind in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::Input,maho_ext_api::EventKind::InputDisposition,maho_ext_api::EventKind::ToolCall] {
            let wiring=wiring.clone(); let actions=actions.clone(); let resolve=resolve.clone();
            api.on(kind,std::sync::Arc::new(move |event,context| {
                let id=context.session_manager.session_id();
                let mut wiring=wiring.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let mut record=None;
                match event {
                    maho_ext_api::ExtensionEvent::SessionStart(_) if !id.is_empty()=>wiring.session_start(id,&context.session_manager.get_entries().into_iter().map(|entry|entry.data).collect::<Vec<_>>()),
                    maho_ext_api::ExtensionEvent::Input(input)=>wiring.input(&serde_json::json!({"type":"input","inputId":input.input_id,"source":if input.source==maho_ext_api::InputSource::Extension{"extension"}else{"interactive"}}),Some(id)),
                    maho_ext_api::ExtensionEvent::InputDisposition{input_id,disposition}=>record=wiring.input_disposition(&serde_json::json!({"type":"input_disposition","inputId":input_id,"disposition":match disposition {maho_ext_api::InputDisposition::Started=>"started",maho_ext_api::InputDisposition::Queued=>"queued",maho_ext_api::InputDisposition::Handled=>"handled",maho_ext_api::InputDisposition::Rejected=>"rejected"}})),
                    maho_ext_api::ExtensionEvent::ToolCall(call)=> {
                        let mut payload=serde_json::json!({"toolName":call.tool_name,"toolCallId":call.tool_call_id,"input":call.input});
                        wiring.inject_provenance(&mut payload,Some(id),resolve(id).as_ref());
                        call.input=payload["input"].clone();
                    },
                    _=>{},
                }
                drop(wiring);
                let actions=actions.clone();
                Box::pin(async move {
                    if let Some(record)=record {actions.append_entry(ACCEPTED_TURNS_ENTRY_TYPE,Some(serde_json::to_value(record).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?))?;}
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
        }
    }
    pub fn session_start(&mut self,id:&str,entries:&[Value]){
        let record=entries.iter().rev().filter(|entry|entry.get("type").and_then(Value::as_str)==Some("custom")&&entry.get("customType").and_then(Value::as_str)==Some(ACCEPTED_TURNS_ENTRY_TYPE)).filter_map(|entry|entry.get("data").cloned()).filter_map(|value|serde_json::from_value::<AcceptedTurnsRecord>(value).ok()).find(|record|record.version==1&&record.session_id==id&&record.prior_user_turns<=MAX_SAFE_INTEGER&&record.session_baseline_turns<=record.prior_user_turns).unwrap_or_else(||AcceptedTurnsRecord{version:1,session_id:id.into(),prior_user_turns:0,session_baseline_turns:0});self.sessions.insert(id.into(),record);
    }
    pub fn input(&mut self,payload:&Value,session_id:Option<&str>){
        if payload.get("type").and_then(Value::as_str)!=Some("input")||payload.get("source").and_then(Value::as_str)==Some("extension"){return;}
        if let Some(input_id)=payload.get("inputId").and_then(Value::as_str).filter(|id|!id.is_empty())&&let Some(session)=session_id.filter(|id|!id.is_empty()){self.pending_inputs.insert(input_id.into(),session.into());}
    }
    pub fn input_disposition(&mut self,payload:&Value)->Option<AcceptedTurnsRecord>{if payload.get("type").and_then(Value::as_str)!=Some("input_disposition"){return None;}let id=payload.get("inputId")?.as_str()?;let session=self.pending_inputs.remove(id)?;if !matches!(payload.get("disposition").and_then(Value::as_str),Some("queued"|"started")){return None;}let state=self.sessions.entry(session.clone()).or_insert(AcceptedTurnsRecord{version:1,session_id:session,prior_user_turns:0,session_baseline_turns:0});state.prior_user_turns+=1;Some(state.clone())}
    pub fn inject_provenance(&self,payload:&mut Value,session_id:Option<&str>,context:Option<&MemoryIdentityContext>){let Some(id)=session_id else{return;};let Some(state)=self.sessions.get(id)else{return;};let Some(context)=context else{return;};if !matches!(payload.get("toolName").and_then(Value::as_str),Some("memory"|"memory_apply_patch"|"mcp_omo-memory_memory"|"mcp_omo-memory_memory_apply_patch")){return;}let call_id=payload.get("toolCallId").and_then(Value::as_str).filter(|id|!id.is_empty()).map(str::to_owned);let Some(input)=payload.get_mut("input").and_then(Value::as_object_mut)else{return;};let mut provenance=serde_json::json!({"sessionId":id,"userTurns":state.prior_user_turns,"identityId":context.identity,"repoPath":context.identity_paths.repo});if let Some(call_id)=call_id{provenance["toolCallId"]=call_id.into();}input.insert("provenance".into(),provenance);}
    pub fn provenance(&self,session_id:&str)->Option<Value>{self.sessions.get(session_id).map(|state|serde_json::json!({"sessionId":session_id,"userTurns":state.prior_user_turns}))}
    pub fn nudge_turns(&self,repo:&GitMemoryRepo,session_id:&str,settings:&ResolvedNudgeSettings)->Result<Option<i64>,GitError>{let Some(state)=self.sessions.get(session_id)else{return Ok(None);};if !settings.enabled{return Ok(None);}let history=if repo.head()?.is_none(){vec![]}else{repo.log(None)?};let saved=history.iter().filter(|commit|commit.trailers.get("Omo-Writer").map(String::as_str)==Some("memory-tool")&&commit.trailers.get("Omo-Session").map(String::as_str)==Some(session_id)).find_map(|commit|commit.trailers.get("Omo-Turn").and_then(|turn|parse_turn(turn))).unwrap_or(state.session_baseline_turns);let pending=i64::from(self.pending_inputs.values().any(|id|id==session_id));let turns=i64::try_from(state.prior_user_turns).unwrap_or(i64::MAX)+pending-i64::try_from(saved).unwrap_or(i64::MAX);Ok((turns>=settings.every_user_turns).then_some(turns))}
}
#[cfg(test)]
mod tests{
    use super::*;
    fn input(id:&str,source:&str)->Value{serde_json::json!({"type":"input","inputId":id,"source":source})}
    fn disposition(id:&str,value:&str)->Value{serde_json::json!({"type":"input_disposition","inputId":id,"disposition":value})}
    #[test]fn handled_input_never_counts_and_removes_pending(){let mut wiring=MemoryNudgeWiring::default();wiring.session_start("session",&[]);wiring.input(&input("handled","interactive"),Some("session"));assert!(wiring.input_disposition(&disposition("handled","handled")).is_none());assert!(wiring.pending_inputs.is_empty());assert_eq!(wiring.provenance("session").unwrap()["userTurns"],0);}
    #[test]fn unbound_provenance_is_noop(){let mut wiring=MemoryNudgeWiring::default();wiring.session_start("session",&[]);let mut payload=serde_json::json!({"toolName":"memory","toolCallId":"call","input":{"command":"create"}});let original=payload.clone();wiring.inject_provenance(&mut payload,Some("session"),None);assert_eq!(payload,original);}
    #[test]fn disabled_nudge_does_not_inspect_repository(){let root=tempfile::tempdir().unwrap();let repo=GitMemoryRepo::open(root.path(),"agent").unwrap();let mut wiring=MemoryNudgeWiring::default();wiring.session_start("session",&[]);assert_eq!(wiring.nudge_turns(&repo,"session",&ResolvedNudgeSettings{enabled:false,every_user_turns:1}).unwrap(),None);}
    #[test]fn only_accepted_interactive_and_rpc_turns_count(){let mut wiring=MemoryNudgeWiring::default();wiring.session_start("session",&[]);for (id,source,state) in [("extension","extension","started"),("reject","interactive","rejected"),("first","interactive","started"),("second","rpc","queued")]{wiring.input(&input(id,source),Some("session"));wiring.input_disposition(&disposition(id,state));}assert_eq!(wiring.provenance("session"),Some(serde_json::json!({"sessionId":"session","userTurns":2})));assert!(wiring.input_disposition(&disposition("second","started")).is_none());}
    #[test]fn hydration_ignores_invalid_and_unrelated_records(){let mut wiring=MemoryNudgeWiring::default();let entries=[serde_json::json!({"type":"custom","customType":ACCEPTED_TURNS_ENTRY_TYPE,"data":{"version":1,"sessionId":"session","priorUserTurns":8,"sessionBaselineTurns":6}}),serde_json::json!({"type":"custom","customType":ACCEPTED_TURNS_ENTRY_TYPE,"data":{"version":1,"sessionId":"session","priorUserTurns":4,"sessionBaselineTurns":8}})];wiring.session_start("session",&entries);assert_eq!(wiring.provenance("session").unwrap()["userTurns"],8);}
    #[test]fn pending_turn_and_committed_save_reset_threshold(){let dir=tempfile::tempdir().unwrap();let repo=GitMemoryRepo::open(dir.path(),"agent").unwrap();repo.init(None).unwrap();let mut wiring=MemoryNudgeWiring::default();wiring.session_start("session",&[]);wiring.input(&input("first","interactive"),Some("session"));wiring.input_disposition(&disposition("first","started"));wiring.input(&input("second","interactive"),Some("session"));let settings=ResolvedNudgeSettings{enabled:true,every_user_turns:2};assert_eq!(wiring.nudge_turns(&repo,"session",&settings).unwrap(),Some(2));wiring.input_disposition(&disposition("second","started"));std::fs::write(dir.path().join("saved.md"),"saved").unwrap();repo.commit_write(&["saved.md"],"save\n\nOmo-Writer: memory-tool\nOmo-Session: session\nOmo-Turn: 2",&memory_core::git::repo_types::GitCommitAuthor{agent_id:"agent".into(),author_name:"agent".into(),author_email:None}).unwrap();assert_eq!(wiring.nudge_turns(&repo,"session",&settings).unwrap(),None);}
    #[test]fn malformed_turn_trailers_rejected(){for value in ["","-1","1.5","1x","9007199254740992"]{assert_eq!(parse_turn(value),None);}assert_eq!(parse_turn("002"),Some(2));}
}
