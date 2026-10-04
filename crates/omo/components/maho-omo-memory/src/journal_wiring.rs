use std::{collections::BTreeMap,path::PathBuf};
use serde_json::Value;
use memory_core::journal::{entries::{ProjectedReasoning,ProjectedToolCall,TranscriptProjection},store::{AppendResult,JournalError,TranscriptJournal,TranscriptJournalOptions}};
fn message(entry:&Value)->Option<(&str,&str,&Value)>{if entry.get("type")?.as_str()? != "message"{return None;}let id=entry.get("id")?.as_str()?;let body=entry.get("message")?;if !body.is_object(){return None;}Some((id,body.get("role")?.as_str()?,body))}
fn text_blocks(content:Option<&Value>)->Vec<String>{content.and_then(Value::as_array).into_iter().flatten().filter(|block|block.get("type").and_then(Value::as_str)==Some("text")).filter_map(|block|block.get("text").and_then(Value::as_str)).filter(|text|!text.trim().is_empty()).map(str::to_owned).collect()}
pub fn project_session_entries(entries:&[Value])->Vec<TranscriptProjection>{
    let mut results=BTreeMap::new();
    for entry in entries{if let Some((_,"toolResult",body))=message(entry)&&let Some(id)=body.get("toolCallId").and_then(Value::as_str){results.insert(id,(text_blocks(body.get("content")).join("\n"),body.get("isError")!=Some(&Value::Bool(true))));}}
    let mut projections=Vec::new();
    for entry in entries{
        let Some((id,role,body))=message(entry)else{continue;};
        let content=body.get("content");
        if role=="user"{
            projections.push(TranscriptProjection::User{message_id:id.into(),text:content.and_then(Value::as_str).map(str::to_owned).unwrap_or_else(||text_blocks(content).join("\n"))});
            continue;
        }
        if role!="assistant"{continue;}
        let texts=content.and_then(Value::as_str).filter(|text|!text.trim().is_empty()).map(|text|vec![text.to_owned()]).unwrap_or_else(||text_blocks(content));let mut reasoning=Vec::new();let mut tools=Vec::new();
        for block in content.and_then(Value::as_array).into_iter().flatten(){match block.get("type").and_then(Value::as_str){Some("thinking")=>{if block.get("redacted")==Some(&Value::Bool(true)){reasoning.push(ProjectedReasoning::redacted());}else if let Some(text)=block.get("thinking").and_then(Value::as_str){reasoning.push(ProjectedReasoning::Text(text.into()));}},Some("toolCall")=>{let Some(call_id)=block.get("id").and_then(Value::as_str)else{continue;};let result=results.get(call_id);tools.push(ProjectedToolCall{call_id:call_id.into(),name:block.get("name").and_then(Value::as_str).map(str::to_owned),args_text:block.get("arguments").map(|value|value.as_str().map(str::to_owned).unwrap_or_else(||value.to_string())),result_text:result.map(|(text,_)|text.clone()),result_ok:result.map(|(_,ok)|*ok)});},_=>{}}}
        projections.push(TranscriptProjection::Assistant{message_id:id.into(),text_blocks:texts,reasoning_blocks:reasoning,tool_calls:tools});
        if let Some(error)=body.get("errorMessage").and_then(Value::as_str).filter(|error|!error.trim().is_empty()){projections.push(TranscriptProjection::Error{message_id:id.into(),text:error.into()});}
    }
    projections
}
pub struct MemoryJournalWiring{transcripts:PathBuf,journals:BTreeMap<String,TranscriptJournal>}
pub type JournalWarning=std::sync::Arc<dyn Fn(&str,&JournalError)+Send+Sync>;
impl MemoryJournalWiring{
    pub fn new(transcripts:PathBuf)->Self{Self{transcripts,journals:BTreeMap::new()}}
    pub fn journal_for(&mut self,session_id:&str)->&TranscriptJournal{self.journals.entry(session_id.into()).or_insert_with(||TranscriptJournal::new(TranscriptJournalOptions::new(self.transcripts.join(session_id))))}
    pub fn reconcile_session(&mut self,session_id:Option<&str>,entries:&[Value],warn:impl FnOnce(&str,&JournalError))->Result<AppendResult,JournalError>{let Some(id)=session_id.filter(|id|!id.is_empty())else{return Ok(AppendResult{appended:0,skipped:0});};match self.journal_for(id).reconcile(&project_session_entries(entries)){Err(error) if matches!(&error,JournalError::LockTimeout(_))||matches!(&error,JournalError::Io(error) if error.kind()==std::io::ErrorKind::AlreadyExists)=>{warn(id,&error);Ok(AppendResult{appended:0,skipped:0})},result=>result}}
    pub fn register(wiring:std::sync::Arc<std::sync::Mutex<Self>>,api:&mut maho_ext_api::ExtensionApi,warn:JournalWarning) {
        for event in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::AgentSettled] {
            let wiring=wiring.clone(); let warn=warn.clone();
            api.on(event,std::sync::Arc::new(move |_,context| {
                let entries=context.session_manager.get_branch().into_iter().map(|entry|entry.data).collect::<Vec<_>>();
                let result=wiring.lock().unwrap_or_else(std::sync::PoisonError::into_inner).reconcile_session(Some(context.session_manager.session_id()),&entries,|id,error|warn(id,error));
                Box::pin(async move {result.map(|_|maho_ext_api::EventResult::None).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))})
            }));
        }
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn block_user_and_split_assistant_text_are_preserved(){let values=project_session_entries(&[serde_json::json!({"type":"message","id":"u","message":{"role":"user","content":[{"type":"text","text":"one"},{"type":"image"},{"type":"text","text":"two"}]}}),serde_json::json!({"type":"message","id":"a","message":{"role":"assistant","content":[{"type":"text","text":"alpha"},{"type":"text","text":"beta"}]}})]);let TranscriptProjection::User{text,..}=&values[0]else{panic!("user")};assert_eq!(text,"one\ntwo");let TranscriptProjection::Assistant{text_blocks,..}=&values[1]else{panic!("assistant")};assert_eq!(text_blocks.as_slice(),["alpha","beta"]);}
    #[test]fn visible_and_redacted_reasoning_preserve_order(){let values=project_session_entries(&[serde_json::json!({"type":"message","id":"a","message":{"role":"assistant","content":[{"type":"thinking","thinking":"visible"},{"type":"thinking","redacted":true},{"type":"text","text":"answer"}]}})]);let TranscriptProjection::Assistant{reasoning_blocks,..}=&values[0]else{panic!("assistant")};assert_eq!(reasoning_blocks.as_slice(),[ProjectedReasoning::Text("visible".into()),ProjectedReasoning::redacted()]);}
    #[test]fn assistant_error_remains_journal_projection(){let values=project_session_entries(&[serde_json::json!({"type":"message","id":"a","message":{"role":"assistant","content":"partial","errorMessage":"aborted"}})]);assert_eq!(values.len(),2);let TranscriptProjection::Error{text,..}=&values[1]else{panic!("error")};assert_eq!(text,"aborted");}
    #[test]fn native_journal_registers_only_start_and_settled(){let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());MemoryJournalWiring::register(std::sync::Arc::new(std::sync::Mutex::new(MemoryJournalWiring::new(Default::default()))),&mut api,std::sync::Arc::new(|_,_|{}));assert_eq!(api.registered.handlers.len(),2);assert!(api.registered.handlers.contains_key(&maho_ext_api::EventKind::SessionStart));assert!(api.registered.handlers.contains_key(&maho_ext_api::EventKind::AgentSettled));}
    #[test]fn three_completed_turns_advance_three_steps(){let root=tempfile::tempdir().unwrap();let mut values=vec![];for index in 0..3{values.push(serde_json::json!({"type":"message","id":format!("u{index}"),"message":{"role":"user","content":"question"}}));values.push(serde_json::json!({"type":"message","id":format!("a{index}"),"message":{"role":"assistant","content":"answer"}}));}let mut wiring=MemoryJournalWiring::new(root.path().into());wiring.reconcile_session(Some("session"),&values,|_,_|panic!("warn")).unwrap();assert_eq!(wiring.journal_for("session").get_state().unwrap().total_completed_steps,3);}
    #[test]fn crash_partial_branch_appends_only_missing_rows(){let root=tempfile::tempdir().unwrap();let values=entries();let mut wiring=MemoryJournalWiring::new(root.path().into());wiring.reconcile_session(Some("session"),&values[..1],|_,_|panic!("warn")).unwrap();let result=MemoryJournalWiring::new(root.path().into()).reconcile_session(Some("session"),&values,|_,_|panic!("warn")).unwrap();assert_eq!(result.appended,3);assert_eq!(result.skipped,1);}
    #[test]fn empty_session_never_creates_journal(){let root=tempfile::tempdir().unwrap();let mut wiring=MemoryJournalWiring::new(root.path().join("absent"));assert_eq!(wiring.reconcile_session(Some(""),&entries(),|_,_|panic!("warn")).unwrap().appended,0);assert!(!root.path().join("absent").exists());}
    fn entries()->Vec<Value>{vec![serde_json::json!({"type":"message","id":"u1","message":{"role":"user","content":"question"}}),serde_json::json!({"type":"message","id":"a1","message":{"role":"assistant","content":[{"type":"text","text":"answer"},{"type":"thinking","redacted":true},{"type":"toolCall","id":"call1","name":"read","arguments":{"path":"/tmp/x"}}]}}),serde_json::json!({"type":"message","id":"tr1","message":{"role":"toolResult","toolCallId":"call1","content":[{"type":"text","text":"file body"}],"isError":true}})]}
    #[test]fn projection_joins_tool_result_and_redacts_reasoning(){let values=project_session_entries(&entries());assert_eq!(values.len(),2);let TranscriptProjection::Assistant{reasoning_blocks,tool_calls,..}=&values[1]else{panic!("assistant")};assert_eq!(reasoning_blocks.as_slice(),[ProjectedReasoning::redacted()]);assert_eq!(tool_calls[0].args_text.as_deref(),Some("{\"path\":\"/tmp/x\"}"));assert_eq!(tool_calls[0].result_text.as_deref(),Some("file body"));assert_eq!(tool_calls[0].result_ok,Some(false));}
    #[test]fn real_journal_restart_reconciles_once(){let dir=tempfile::tempdir().unwrap();let values=entries();let mut before=MemoryJournalWiring::new(dir.path().into());let first=before.reconcile_session(Some("session"),&values,|_,_|panic!("warning")).unwrap();assert_eq!(first.appended,4);let mut after=MemoryJournalWiring::new(dir.path().into());let second=after.reconcile_session(Some("session"),&values,|_,_|panic!("warning")).unwrap();assert_eq!(second,AppendResult{appended:0,skipped:4});assert_eq!(after.journal_for("session").get_state().unwrap().total_completed_steps,1);}
    #[test]fn malformed_entries_and_missing_session_are_noop(){let values=[Value::Null,serde_json::json!({"type":"compaction"}),serde_json::json!({"type":"message","id":"x","message":{"role":"custom","content":"hidden"}})];assert!(project_session_entries(&values).is_empty());let dir=tempfile::tempdir().unwrap();assert_eq!(MemoryJournalWiring::new(dir.path().join("absent")).reconcile_session(None,&entries(),|_,_|panic!("warn")).unwrap().appended,0);assert!(!dir.path().join("absent").exists());}
    #[test]fn tool_only_assistant_adds_no_step(){let dir=tempfile::tempdir().unwrap();let values=[serde_json::json!({"type":"message","id":"a1","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"read"}]}})];let mut wiring=MemoryJournalWiring::new(dir.path().into());assert_eq!(wiring.reconcile_session(Some("session"),&values,|_,_|panic!("warn")).unwrap().appended,1);assert_eq!(wiring.journal_for("session").get_state().unwrap().total_completed_steps,0);}
}
