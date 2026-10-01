use serde::{Serialize,Deserialize};
pub const SOUL_UPDATED_ENTRY_TYPE:&str="omo-memory:soul-updated";
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SoulUpdatedRecord{pub sha:String,pub subject:String,pub affected_paths:Vec<String>}
pub trait SoulNoticeApi{fn register_entry_renderer(&mut self,entry_type:&str);fn append_entry(&mut self,entry_type:&str,record:&SoulUpdatedRecord);}
pub fn on_soul_commit(api:Option<&mut dyn SoulNoticeApi>,edit_notice:bool,commit:&SoulUpdatedRecord){
    if !edit_notice||!commit.affected_paths.iter().any(|path|path=="system/persona.md"||path=="system/identity.md"){return;}
    if let Some(api)=api{api.append_entry(SOUL_UPDATED_ENTRY_TYPE,commit);}
}
pub fn consume_soul_tool_result(api:&mut dyn SoulNoticeApi,context:&crate::context::MemoryIdentityContext,edit_notice:bool,payload:&serde_json::Value)->Result<(),crate::worker::run_artifacts::ArtifactError>{
    if payload.get("type").and_then(serde_json::Value::as_str)!=Some("tool_result"){return Ok(());}
    if !matches!(payload.get("toolName").and_then(serde_json::Value::as_str),Some("mcp_omo-memory_memory"|"mcp_omo-memory_memory_apply_patch")){return Ok(());}
    let Some(call)=payload.get("toolCallId").and_then(serde_json::Value::as_str).filter(|call|!call.is_empty())else{return Ok(());};
    if let Some(receipt)=crate::tool_receipts::consume_tool_receipt(&context.identity_paths.tool_receipts,call)?{on_soul_commit(Some(api),edit_notice,&SoulUpdatedRecord{sha:receipt.sha,subject:receipt.subject,affected_paths:receipt.affected_paths});}
    Ok(())
}
pub fn render_soul_updated_entry(record:&SoulUpdatedRecord)->Vec<String>{
    use senpi_task::renderer_text::normalize_renderer_text as normalize;
    let short=String::from_utf16_lossy(&record.sha.encode_utf16().take(7).collect::<Vec<_>>());
    std::iter::once(format!("memory soul updated {}: {}",normalize(&short),normalize(&record.subject))).chain(record.affected_paths.iter().map(|path|normalize(path))).collect()
}
#[cfg(test)]
mod tests{
    use super::*;
    #[derive(Default)]struct Api{entries:Vec<SoulUpdatedRecord>,renderers:Vec<String>}
    impl SoulNoticeApi for Api{fn register_entry_renderer(&mut self,entry_type:&str){self.renderers.push(entry_type.into());}fn append_entry(&mut self,entry_type:&str,record:&SoulUpdatedRecord){assert_eq!(entry_type,SOUL_UPDATED_ENTRY_TYPE);self.entries.push(record.clone());}}
    fn commit()->SoulUpdatedRecord{SoulUpdatedRecord{sha:"a1b2c3d4e5f6".into(),subject:"rewrite persona".into(),affected_paths:vec!["system/persona.md".into()]}}
    #[test]fn soul_commit_emits_metadata_only_when_enabled(){let mut api=Api::default();let commit=commit();on_soul_commit(Some(&mut api),true,&commit);assert_eq!(api.entries,[commit]);}
    #[test]fn nonsoul_is_silent(){let mut api=Api::default();let mut commit=commit();commit.affected_paths=vec!["notes/facts.md".into()];on_soul_commit(Some(&mut api),true,&commit);assert!(api.entries.is_empty());}
    #[test]fn disabled_is_silent(){let mut api=Api::default();on_soul_commit(Some(&mut api),false,&commit());assert!(api.entries.is_empty());}
    #[test]fn renderer_registration_uses_entry_type(){let mut api=Api::default();api.register_entry_renderer(SOUL_UPDATED_ENTRY_TYPE);assert_eq!(api.renderers,[SOUL_UPDATED_ENTRY_TYPE]);}
    #[test]fn actual_mcp_receipt_is_consumed_once(){let root=tempfile::tempdir().unwrap();let paths=memory_core::identity::layout::build_identity_paths(root.path(),"agent");let context=crate::context::MemoryIdentityContext::new("agent".into(),paths,crate::binding::MemorySessionBinding{identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:0.0});let commit=commit();crate::tool_receipts::write_tool_receipt(&context.identity_paths.tool_receipts,&crate::tool_receipts::MemoryToolReceipt{version:1,tool_call_id:"call".into(),sha:commit.sha.clone(),subject:commit.subject.clone(),affected_paths:commit.affected_paths.clone()}).unwrap();let payload=serde_json::json!({"type":"tool_result","toolName":"mcp_omo-memory_memory","toolCallId":"call"});let mut api=Api::default();for _ in 0..2{consume_soul_tool_result(&mut api,&context,true,&payload).unwrap();}assert_eq!(api.entries,[commit]);assert!(!crate::tool_receipts::tool_receipt_path(&context.identity_paths.tool_receipts,"call").exists());}
}
