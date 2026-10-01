use std::{collections::BTreeSet, sync::{Arc, Mutex, OnceLock}};
use maho_ext_api::*;
use crate::generated_directive::SENPI_ULTRAWORK_DIRECTIVE;

pub const ULTRAWORK_DISABLED_FLAG: &str = "omo-senpi-ultrawork-disabled";
pub const ULTRAWORK_CUSTOM_TYPE: &str = "omo-ultrawork:directive";
pub const ULTRAWORK_REMINDER: &str = "<omo-ultrawork-reminder>ultrawork mode is already armed for this session - the ultrawork directive above remains binding; re-read it and continue.</omo-ultrawork-reminder>";

#[derive(Default)]
pub struct SessionArming {
    armed: BTreeSet<Option<String>>,
    compact_pending: BTreeSet<Option<String>>,
    current: Option<String>,
}
impl SessionArming {
    pub fn track_session(&mut self, id: Option<&str>) { self.current=id.map(str::to_owned); }
    pub fn current_session_id(&self) -> Option<&str> { self.current.as_deref() }
    pub fn rearm_on_compact(&mut self, id: Option<&str>) { let target=id.map(str::to_owned).or_else(||self.current.clone()); self.armed.remove(&target); self.compact_pending.insert(target); }
    pub fn is_armed(&self, id: Option<&str>) -> bool { self.armed.contains(&id.map(str::to_owned)) }
    pub fn is_compact_rearm_pending(&self,id:Option<&str>) -> bool { self.compact_pending.contains(&id.map(str::to_owned)) }
    pub fn mark_armed(&mut self,id:Option<&str>) { let target=id.map(str::to_owned); self.compact_pending.remove(&target); self.armed.insert(target); }
}
pub fn shared_session_arming() -> Arc<Mutex<SessionArming>> {
    static ARMING:OnceLock<Arc<Mutex<SessionArming>>>=OnceLock::new();
    Arc::clone(ARMING.get_or_init(||Arc::new(Mutex::new(SessionArming::default()))))
}
pub fn is_ultrawork_input(text:&str) -> bool {
    let lower=text.to_ascii_lowercase();
    lower.contains("ultrawork") || lower.match_indices("ulw").any(|(index,_)| !lower[index+3..].starts_with('-'))
}
pub fn already_embedded(text:&str) -> bool { text.contains("<ultrawork-mode>") && text.contains("</ultrawork-mode>") }
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub struct ArmingSnapshot { pub was_armed:bool,pub compact_rearm_pending:bool }
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct UltraworkClassification {
    pub matched_ulw:bool,pub matched_ultrawork:bool,pub occurrence_count:usize,
    pub effective:bool,pub stage:&'static str,pub route:&'static str,pub suppression_reason:&'static str,
}
pub fn arming_snapshot(id:Option<&str>)->ArmingSnapshot {
    let shared=shared_session_arming();let a=shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    ArmingSnapshot{was_armed:a.is_armed(id),compact_rearm_pending:a.is_compact_rearm_pending(id)}
}
pub fn classify_ultrawork_input(text:&str,source:InputSource,snapshot:ArmingSnapshot)->UltraworkClassification {
    let lower=text.to_ascii_lowercase();let mut matches=Vec::new();let mut offset=0;
    while offset<lower.len() {
        let rest=&lower[offset..];let full=rest.find("ultrawork");let short=rest.match_indices("ulw").find(|(i,_)|!rest[i+3..].starts_with('-')).map(|(i,_)|i);
        let next=match (full,short) { (None,None)=>break,(Some(i),None)=>(i,true),(None,Some(i))=>(i,false),(Some(a),Some(b))=>if a<=b {(a,true)}else{(b,false)} };
        let start=offset+next.0;matches.push((start,next.1));offset=start+if next.1 {9}else{3};
    }
    let mut result=UltraworkClassification{matched_ulw:matches.iter().any(|(_,full)|!*full),matched_ultrawork:matches.iter().any(|(_,full)|*full),occurrence_count:matches.len(),effective:false,stage:"none",route:"none",suppression_reason:"none"};
    if source==InputSource::Extension {result.suppression_reason="extension_source";return result;}
    if matches.is_empty() {result.suppression_reason="no_keyword";return result;}
    if already_embedded(text) {result.route="embedded_directive";result.suppression_reason="embedded_directive";return result;}
    result.route="direct";
    if let Some(rest)=text.strip_prefix("/skill:") {
        let (name,_)=rest.split_once(' ').unwrap_or((rest,""));
        if name=="ultrawork" {result.route="skill_expansion";result.suppression_reason="skill_expansion";return result;}
        let args_start=text.find(' ').map_or(text.len(),|i|i+1);
        if !matches.iter().any(|(i,_)|*i>=args_start) {result.route="none";result.suppression_reason="skill_name_only";return result;}
        result.route="skill_args";
    }
    result.effective=true;result.stage=if snapshot.compact_rearm_pending {"post_compact_rearm"}else if snapshot.was_armed {"remention"}else{"first_arm"};result
}
pub fn skill_invocation_suppressed(text:&str) -> bool {
    let Some(rest)=text.strip_prefix("/skill:") else { return false; };
    let (name,args)=rest.split_once(' ').unwrap_or((rest,""));
    name=="ultrawork" || !is_ultrawork_input(args)
}
pub struct UltraworkComponent { pub arming: Arc<Mutex<SessionArming>> }
impl Default for UltraworkComponent { fn default() -> Self { Self{arming:shared_session_arming()} } }
impl Extension for UltraworkComponent {
    fn register(&self,api:&mut ExtensionApi) {
        for kind in [EventKind::SessionStart,EventKind::SessionBeforeSwitch,EventKind::SessionCompact] {
            let arming=Arc::clone(&self.arming);
            api.on(kind,Arc::new(move |event,ctx| { let arming=Arc::clone(&arming); Box::pin(async move {
                let id=ctx.session_manager.session_id(); let id=(!id.is_empty()).then_some(id);
                let mut arming=arming.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if matches!(event,ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected{..})) { return Ok(EventResult::None); }
                if matches!(event,ExtensionEvent::SessionCompact(_)) { arming.rearm_on_compact(id); } else { arming.track_session(id); }
                Ok(EventResult::None)
            }) }));
        }
        let arming=Arc::clone(&self.arming); let runtime=api.runtime.clone();
        api.on(EventKind::Input,Arc::new(move |event,ctx| { let arming=Arc::clone(&arming); let runtime=runtime.clone(); Box::pin(async move {
            let ExtensionEvent::Input(input)=event else { return Ok(EventResult::None); };
            if runtime.get_flag(ULTRAWORK_DISABLED_FLAG)==Some(FlagValue::Boolean(true)) || input.source==InputSource::Extension || !is_ultrawork_input(&input.text) { return Ok(EventResult::Input(InputEventResult::Continue)); }
            let id=ctx.session_manager.session_id();
            let mut arming=arming.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let id=if id.is_empty() { arming.current.clone() } else { Some(id.into()) };
            if already_embedded(&input.text) || input.text.strip_prefix("/skill:").is_some_and(|s| s.split(' ').next()==Some("ultrawork")) { arming.mark_armed(id.as_deref()); return Ok(EventResult::Input(InputEventResult::Continue)); }
            if skill_invocation_suppressed(&input.text) { return Ok(EventResult::Input(InputEventResult::Continue)); }
            let content=if arming.is_armed(id.as_deref()) { ULTRAWORK_REMINDER } else { SENPI_ULTRAWORK_DIRECTIVE.trim_end_matches('\n') };
            arming.mark_armed(id.as_deref());
            if input.streaming_behavior.is_some() { return Ok(EventResult::Input(InputEventResult::Transform{text:format!("{}\n{content}",input.text),images:None})); }
            let api=ExtensionApi::new(LoadedExtension::new("ultrawork",std::path::PathBuf::new(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
            api.send_message(CustomMessage{custom_type:ULTRAWORK_CUSTOM_TYPE.into(),content:vec![ToolContent::text(content)],display:false,details:None},SendMessageOptions::default())?;
            Ok(EventResult::Input(InputEventResult::Continue))
        }) }));
    }
}
