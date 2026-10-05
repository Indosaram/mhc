use std::path::Path;
use serde::{Serialize,Deserialize};
pub const REFLECTION_HEALTH_ENTRY_TYPE:&str="senpi-memory.health";
#[derive(Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ReflectionHealthEntry{pub schema_version:u32,pub identity:String,pub streak:usize,pub fingerprint:String,pub last_reason:String,#[serde(skip_serializing_if="Option::is_none")]pub last_detail:Option<String>,#[serde(rename="sinceISO")]pub since_iso:String,pub recommendation:String}
pub trait ReflectionHealthLiveSession{fn session_id(&self)->&str;fn has_ui(&self)->bool;fn append_entry(&mut self,name:&str,entry:&ReflectionHealthEntry);fn notify(&mut self,message:&str,level:&str);}
pub fn register_reflection_health_renderer(api:&mut maho_ext_api::ExtensionApi,theme:super::completion_renderers::ResolveEntryTheme){
    api.register_entry_renderer(REFLECTION_HEALTH_ENTRY_TYPE,std::sync::Arc::new(move |entry,options,native_theme|{
        use super::entry_renderers::*;
        let health:ReflectionHealthEntry=serde_json::from_value(entry.data.get("data")?.clone()).ok()?;
        let reason=format!("reason {}",normalize_renderer_text(&health.last_reason));
        let detail=optional_renderer_text(health.last_detail.as_deref()).map(|detail|detail_excerpt(&detail));
        let since=format!("since {}",normalize_renderer_text(&health.since_iso));let identity=format!("identity {}",normalize_renderer_text(&health.identity));
        Some(Box::new(NoticeComponent{spec:NoticeSpec{glyph:"✗".into(),title:format!("Memory reflection failing · {} run{} in a row",health.streak,if health.streak==1{""}else{"s"}),tone:"error".into(),why:normalize_renderer_text(&health.recommendation),extra:vec![],detail:Some(join_fields(&[Some(&reason),detail.as_deref(),Some(&since),Some(&identity)]))},expanded:options.expanded,theme:theme(native_theme)}))
    }),Default::default());
}
pub fn emit_reflection_health_alert(completions:&Path,identity:&str,live:Option<&mut dyn ReflectionHealthLiveSession>,once:&mut dyn FnMut(&str)->bool,now:i64)->bool{
    let Some(live)=live.filter(|live|live.has_ui())else{return false;};let health=super::health::read_reflection_health(completions,100,now);
    if health.streak<3||health.fingerprint.is_empty()||health.recent_failure_fingerprints.iter().filter(|item|*item==&health.fingerprint).count()<2{return false;}
    if !once(&format!("{}:{}",live.session_id(),health.fingerprint)){return false;}
    let failure=health.last_failure;let recommendation=super::remediation::reflection_remediation(failure.as_ref().map(|failure|failure.reason.as_str()),failure.as_ref().and_then(|failure|failure.detail.as_deref())).to_owned();
    let entry=ReflectionHealthEntry{schema_version:1,identity:identity.into(),streak:health.streak,fingerprint:health.fingerprint,last_reason:failure.as_ref().map(|failure|failure.reason.clone()).unwrap_or_else(||"failed".into()),last_detail:failure.as_ref().and_then(|failure|failure.detail.clone()),since_iso:health.streak_since_iso.or_else(||failure.map(|failure|failure.finished_at)).unwrap_or_else(||"1970-01-01T00:00:00.000Z".into()),recommendation};
    live.append_entry(REFLECTION_HEALTH_ENTRY_TYPE,&entry);live.notify(&format!("Memory reflection has failed {} times ({}). {}",entry.streak,entry.fingerprint,entry.recommendation),"warning");true
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn alert_reads_default_hundred_record_history_not_footer_limit(){
        let root=tempfile::tempdir().unwrap();for index in 0..30{std::fs::write(root.path().join(format!("run-{index}.json")),serde_json::to_vec(&serde_json::json!({"runId":format!("run-{index}"),"outcome":"failed","reason":"child_exit","detail":"stable","finishedAt":"2026-08-12T00:00:00Z"})).unwrap()).unwrap();}
        let mut session=live("session");assert!(emit_reflection_health_alert(root.path(),"agent",Some(&mut session),&mut |_|true,now()));assert_eq!(session.entries[0].streak,30);
    }
    #[test]fn native_health_renderer_registers_and_renders_details(){
        struct Theme;
        impl super::super::entry_renderers::EntryRenderTheme for Theme{fn fg(&self,tone:&str,text:&str)->String{format!("<{tone}>{text}</{tone}>")}fn italic(&self,text:&str)->String{text.into()}}
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        register_reflection_health_renderer(&mut api,std::sync::Arc::new(|_|std::sync::Arc::new(Theme)));
        let data=ReflectionHealthEntry{schema_version:1,identity:"agent".into(),streak:3,fingerprint:"child_exit:stable".into(),last_reason:"child_exit".into(),last_detail:Some("stable".into()),since_iso:"2026-10-02T00:00:00.000Z".into(),recommendation:"Check the worker logs".into()};
        let entry=maho_ext_api::SessionEntry{id:"entry".into(),parent_id:None,timestamp:"now".into(),kind:"custom".into(),data:serde_json::json!({"data":data})};
        let renderer=&api.registered.entry_renderers[REFLECTION_HEALTH_ENTRY_TYPE];
        let mut collapsed=renderer(&entry,&maho_ext_api::EntryRenderOptions{expanded:false},&Default::default()).unwrap();let lines=collapsed.render(120);assert_eq!(lines.len(),2);assert!(lines[0].starts_with("<error>"));
        let mut expanded=renderer(&entry,&maho_ext_api::EntryRenderOptions{expanded:true},&Default::default()).unwrap();assert_eq!(expanded.render(120).len(),3);
        assert!(renderer(&maho_ext_api::SessionEntry{data:serde_json::Value::Null,..entry},&Default::default(),&Default::default()).is_none());
    }
    struct Live{session:String,ui:bool,entries:Vec<ReflectionHealthEntry>,warnings:usize}
    impl ReflectionHealthLiveSession for Live{fn session_id(&self)->&str{&self.session}fn has_ui(&self)->bool{self.ui}fn append_entry(&mut self,name:&str,entry:&ReflectionHealthEntry){assert_eq!(name,REFLECTION_HEALTH_ENTRY_TYPE);self.entries.push(ReflectionHealthEntry{schema_version:entry.schema_version,identity:entry.identity.clone(),streak:entry.streak,fingerprint:entry.fingerprint.clone(),last_reason:entry.last_reason.clone(),last_detail:entry.last_detail.clone(),since_iso:entry.since_iso.clone(),recommendation:entry.recommendation.clone()});}fn notify(&mut self,_:&str,level:&str){assert_eq!(level,"warning");self.warnings+=1;}}
    fn live(session:&str)->Live{Live{session:session.into(),ui:true,entries:vec![],warnings:0}}
    fn seed(root:&Path,count:usize,different:bool){for index in 0..count{std::fs::write(root.join(format!("run-{index}.json")),serde_json::to_vec(&serde_json::json!({"runId":format!("run-{index}"),"outcome":"failed","reason":"child_exit","detail":if different{format!("detail-{index}")}else{"stable".into()},"finishedAt":format!("2026-08-12T0{index}:00:00.000Z")})).unwrap()).unwrap();}}
    fn now()->i64{chrono::DateTime::parse_from_rfc3339("2026-08-12T03:00:00.000Z").unwrap().timestamp_millis()}
    #[test]fn below_three_failures_does_not_claim_alert(){let root=tempfile::tempdir().unwrap();seed(root.path(),2,false);let mut live=live("session");assert!(!emit_reflection_health_alert(root.path(),"agent",Some(&mut live),&mut |_|panic!("guard must not run"),now()));assert!(live.entries.is_empty());}
    #[test]fn distinct_failures_do_not_claim_alert(){let root=tempfile::tempdir().unwrap();seed(root.path(),3,true);let mut live=live("session");assert!(!emit_reflection_health_alert(root.path(),"agent",Some(&mut live),&mut |_|panic!("guard must not run"),now()));assert_eq!(live.warnings,0);}
    #[test]fn already_claimed_fingerprint_does_not_append(){let root=tempfile::tempdir().unwrap();seed(root.path(),3,false);let mut live=live("session");assert!(!emit_reflection_health_alert(root.path(),"agent",Some(&mut live),&mut |key|{assert_eq!(key,"session:child_exit:stable");false},now()));assert!(live.entries.is_empty());assert_eq!(live.warnings,0);}
    #[test]fn stale_failure_streak_does_not_alert(){let root=tempfile::tempdir().unwrap();seed(root.path(),3,false);let mut live=live("session");assert!(!emit_reflection_health_alert(root.path(),"agent",Some(&mut live),&mut |_|panic!("guard must not run"),now()+10*86_400_000));assert!(live.entries.is_empty());}
    #[test]fn stable_streak_once_per_session_and_entry_metadata(){let root=tempfile::tempdir().unwrap();seed(root.path(),3,false);let mut seen=std::collections::BTreeSet::new();let mut once=|key:&str|seen.insert(key.to_owned());let mut first=live("one");assert!(emit_reflection_health_alert(root.path(),"agent",Some(&mut first),&mut once,now()));assert!(!emit_reflection_health_alert(root.path(),"agent",Some(&mut first),&mut once,now()));assert_eq!(first.warnings,1);assert_eq!(first.entries.len(),1);let entry=&first.entries[0];assert_eq!(entry.identity,"agent");assert_eq!(entry.streak,3);assert_eq!(entry.fingerprint,"child_exit:stable");assert_eq!(entry.last_detail.as_deref(),Some("stable"));let mut second=live("two");assert!(emit_reflection_health_alert(root.path(),"agent",Some(&mut second),&mut once,now()));}
    #[test]fn threshold_unstable_and_stale_suppressed(){for (count,different,offset) in [(2,false,0),(3,true,0),(3,false,10*86400000)]{let root=tempfile::tempdir().unwrap();seed(root.path(),count,different);let mut live=live("session");assert!(!emit_reflection_health_alert(root.path(),"agent",Some(&mut live),&mut |_|panic!("guard must not run"),now()+offset));assert!(live.entries.is_empty());assert_eq!(live.warnings,0);}}
    #[test]fn missing_ui_does_not_read_or_emit(){let mut live=live("session");live.ui=false;assert!(!emit_reflection_health_alert(Path::new("missing"),"agent",Some(&mut live),&mut |_|panic!("guard must not run"),now()));assert!(!emit_reflection_health_alert(Path::new("missing"),"agent",None,&mut |_|panic!("guard must not run"),now()));assert!(live.entries.is_empty());}
}
