use std::{collections::BTreeSet, sync::{Arc, Mutex, OnceLock}};
use maho_ext_api::*;
use crate::generated_directive::{SENPI_ASTRA_ULTRAWORK_DIRECTIVE, SENPI_ULTRAWORK_DIRECTIVE};

pub const ULTRAWORK_DISABLED_FLAG: &str = "omo-senpi-ultrawork-disabled";
pub const ULTRAWORK_CUSTOM_TYPE: &str = "omo-ultrawork:directive";
pub const ULTRAWORK_REMINDER: &str = "<omo-ultrawork-reminder>ultrawork mode is already armed for this session - the ultrawork directive above remains binding; re-read it and continue.</omo-ultrawork-reminder>";

#[derive(Default)]
pub struct SessionArming { armed: BTreeSet<Option<String>>, compact_pending: BTreeSet<Option<String>>, current: Option<String> }
impl SessionArming {
    pub fn track_session(&mut self,id:Option<&str>){self.current=id.map(str::to_owned);}
    pub fn current_session_id(&self)->Option<&str>{self.current.as_deref()}
    pub fn rearm_on_compact(&mut self,id:Option<&str>){let target=id.map(str::to_owned).or_else(||self.current.clone());self.armed.remove(&target);self.compact_pending.insert(target);}
    pub fn is_armed(&self,id:Option<&str>)->bool{self.armed.contains(&id.map(str::to_owned))}
    pub fn is_compact_rearm_pending(&self,id:Option<&str>)->bool{self.compact_pending.contains(&id.map(str::to_owned))}
    pub fn mark_armed(&mut self,id:Option<&str>){let target=id.map(str::to_owned);self.compact_pending.remove(&target);self.armed.insert(target);}
}
pub fn shared_session_arming()->Arc<Mutex<SessionArming>>{static ARMING:OnceLock<Arc<Mutex<SessionArming>>>=OnceLock::new();Arc::clone(ARMING.get_or_init(||Arc::new(Mutex::new(SessionArming::default()))))}

pub fn is_ultrawork_input(text:&str)->bool{!visible_invocations(text).is_empty()}
fn keyword_matches(text:&str)->Vec<(usize,usize,bool)>{let mut found=Vec::new();let mut offset=0;while offset<text.len(){let rest=&text[offset..];let full=rest.find("ultrawork").map(|i|(i,9,true));let short=rest.match_indices("ulw").find(|(i,_)|!rest[i+3..].starts_with('-')).map(|(i,_)|(i,3,false));let next=match(full,short){(None,None)=>break,(Some(v),None)|(None,Some(v))=>v,(Some(a),Some(b))=>if a.0<=b.0{a}else{b}};let start=offset+next.0;let end=start+next.1;found.push((start,end,next.2));offset=end;}found}
fn is_invocation_word(text:&str,start:usize,end:usize)->bool{let before=text[..start].chars().next_back();let after=text[end..].chars().next();let word=|c:char|c.is_alphanumeric()||c=='_';!(before.is_some_and(word)||after.is_some_and(word)||after==Some('.')&&text[end+1..].chars().next().is_some_and(|c|c.is_alphanumeric()||c=='_'))}
fn visible_invocations(text:&str)->Vec<(usize,usize,bool)>{
    let lower_text=text.to_ascii_lowercase();
    let mut visible=vec![true;text.len()];
    for(open,close)in[("<ultrawork-mode>","</ultrawork-mode>"),("<omo-ultrawork-reminder>","</omo-ultrawork-reminder>")]{
        let mut cursor=0;
        while let Some(a)=lower_text[cursor..].find(open){let start=cursor+a;let Some(b)=lower_text[start+open.len()..].find(close)else{break};let end=start+open.len()+b+close.len();visible[start..end].fill(false);cursor=end;}
    }
    let mut offset=0;let mut result=Vec::new();
    for line in text.split_inclusive('\n'){
        let trimmed=line.trim_start();
        if trimmed.starts_with('>'){visible[offset..offset+line.len()].fill(false);offset+=line.len();continue;}
        let relay=trimmed.starts_with('[')&&trimmed.find(']').is_some_and(|i|i>1)||trimmed.split_once(" (").is_some_and(|(name,rest)|!name.is_empty()&&rest.contains("), ")&&rest.ends_with(':'));
        if relay{visible[offset..].fill(false);break;}
        let lower=line.to_ascii_lowercase();
        for(start,end,full)in keyword_matches(&lower){
            if !visible[offset+start..offset+end].iter().all(|b|*b)||!is_invocation_word(&lower,start,end){continue;}
            let prefix=lower_text[..offset+start].to_owned();let before=prefix.chars().rev().find(|c|!c.is_whitespace());
            let leading_dash=before==Some('-')&&prefix.split_whitespace().next_back().is_none_or(|part|part.ends_with('-'));
            let quote_open=prefix.chars().filter(|c|matches!(c,'`'|'"'|'\''|'“'|'‘')).count();
            if quote_open%2==1||leading_dash||prefix.ends_with("mass-")||prefix.ends_with('.') {continue;}
            result.push((offset+start,offset+end,full));
        }
        offset+=line.len();
    }
    result
}
pub fn already_embedded(text:&str)->bool{text.contains("<ultrawork-mode>")&&text.contains("</ultrawork-mode>")}

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]pub struct ArmingSnapshot{pub was_armed:bool,pub compact_rearm_pending:bool}
#[derive(Clone,Debug,PartialEq,Eq)]pub struct UltraworkClassification{pub matched_ulw:bool,pub matched_ultrawork:bool,pub occurrence_count:usize,pub effective:bool,pub stage:&'static str,pub route:&'static str,pub suppression_reason:&'static str}
pub fn arming_snapshot(id:Option<&str>)->ArmingSnapshot{let a=shared_session_arming();let a=a.lock().unwrap_or_else(std::sync::PoisonError::into_inner);ArmingSnapshot{was_armed:a.is_armed(id),compact_rearm_pending:a.is_compact_rearm_pending(id)}}
pub fn classify_ultrawork_input(text:&str,source:InputSource,snapshot:ArmingSnapshot)->UltraworkClassification{
    let visible=visible_invocations(text);let lower=text.to_ascii_lowercase();let all=keyword_matches(&lower);let mut result=UltraworkClassification{matched_ulw:visible.iter().any(|(_,_,full)|!*full),matched_ultrawork:visible.iter().any(|(_,_,full)|*full),occurrence_count:visible.len(),effective:false,stage:"none",route:"none",suppression_reason:"none"};if source==InputSource::Extension{result.suppression_reason="extension_source";return result;}
    if visible.is_empty(){result.suppression_reason=if all.iter().any(|(s,e,_)|!is_invocation_word(&lower,*s,*e)){"identifier_reference"}else if !all.is_empty(){"negated_mention"}else{"no_keyword"};return result;}
    if already_embedded(text){result.route="embedded_directive";result.suppression_reason="embedded_directive";return result;}result.route="direct";if let Some(rest)=text.strip_prefix("/skill:"){let(name,_)=rest.split_once(' ').unwrap_or((rest,""));if name=="ultrawork"{result.route="skill_expansion";result.suppression_reason="skill_expansion";return result;}let args=text.find(' ').map_or(text.len(),|i|i+1);if !visible.iter().any(|(i,_,_)|*i>=args){result.route="none";result.suppression_reason="skill_name_only";return result;}result.route="skill_args";}result.effective=true;result.stage=if snapshot.compact_rearm_pending{"post_compact_rearm"}else if snapshot.was_armed{"remention"}else{"first_arm"};result
}
pub fn skill_invocation_suppressed(text:&str)->bool{let Some(rest)=text.strip_prefix("/skill:")else{return false;};let(name,args)=rest.split_once(' ').unwrap_or((rest,""));name=="ultrawork"||!is_ultrawork_input(args)}
pub struct UltraworkComponent{pub arming:Arc<Mutex<SessionArming>>}
impl Default for UltraworkComponent{fn default()->Self{Self{arming:shared_session_arming()}}}
impl Extension for UltraworkComponent{
 fn register(&self,api:&mut ExtensionApi){for kind in[EventKind::SessionStart,EventKind::SessionBeforeSwitch,EventKind::SessionCompact]{let arming=Arc::clone(&self.arming);api.on(kind,Arc::new(move|event,ctx|{let arming=Arc::clone(&arming);Box::pin(async move{let id=ctx.session_manager.session_id();let id=(!id.is_empty()).then_some(id);let mut arming=arming.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if matches!(event,ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected{..})){return Ok(EventResult::None);}
    if matches!(event,ExtensionEvent::SessionCompact(_)){arming.rearm_on_compact(id);}else{arming.track_session(id);}Ok(EventResult::None)})}));}
  api.on(EventKind::Context,Arc::new(|event,ctx|Box::pin(async move{let ExtensionEvent::Context{messages,..}=event else{return Ok(EventResult::None);};let changed=reselect_context_directive(messages,ctx.model.as_ref().map(|m|m.id.as_str()));if changed{Ok(EventResult::Context{messages:Some(messages.clone())})}else{Ok(EventResult::None)}})));
  let arming=Arc::clone(&self.arming);let runtime=api.runtime.clone();api.on(EventKind::Input,Arc::new(move|event,ctx|{let arming=Arc::clone(&arming);let runtime=runtime.clone();Box::pin(async move{let ExtensionEvent::Input(input)=event else{return Ok(EventResult::None);};if runtime.get_flag(ULTRAWORK_DISABLED_FLAG)==Some(FlagValue::Boolean(true))||input.source==InputSource::Extension||!is_ultrawork_input(&input.text){return Ok(EventResult::Input(InputEventResult::Continue));}let id=ctx.session_manager.session_id();let mut arming=arming.lock().unwrap_or_else(std::sync::PoisonError::into_inner);let id=if id.is_empty(){arming.current.clone()}else{Some(id.into())};let directive=directive_for_model(ctx.model.as_ref().map(|m|m.id.as_str()));if already_embedded(&input.text)||input.text.strip_prefix("/skill:").is_some_and(|s|s.split(' ').next()==Some("ultrawork")){arming.mark_armed(id.as_deref());return Ok(EventResult::Input(InputEventResult::Continue));}
    if skill_invocation_suppressed(&input.text){return Ok(EventResult::Input(InputEventResult::Continue));}let content=if arming.is_armed(id.as_deref()){ULTRAWORK_REMINDER}else{directive};arming.mark_armed(id.as_deref());if input.streaming_behavior.is_some(){return Ok(EventResult::Input(InputEventResult::Transform{text:format!("{}\n{content}",input.text),images:None}));}let api=ExtensionApi::new(LoadedExtension::new("ultrawork",std::path::PathBuf::new(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);api.send_message(CustomMessage{custom_type:ULTRAWORK_CUSTOM_TYPE.into(),content:vec![ToolContent::text(content)],display:false,details:None},SendMessageOptions::default())?;Ok(EventResult::Input(InputEventResult::Continue))})}));}
}
pub fn reselect_context_directive(messages:&mut Vec<AgentMessage>,model_id:Option<&str>)->bool{let directive=directive_for_model(model_id);let mut selected=false;for message in messages{if let AgentMessage::Llm(maho_ai::types::Message::User(user))=message{match &mut user.content{maho_ai::types::UserContent::Text(text)=>if let Some(next)=select_directive_text(text,directive){*text=next;selected=true;},maho_ai::types::UserContent::Blocks(blocks)=>for block in blocks{if let maho_ai::types::ContentBlock::Text(text)=block&&let Some(next)=select_directive_text(&text.text,directive){text.text=next;selected=true;}}}}}selected}
pub fn directive_for_model(model_id:Option<&str>)->&'static str{let astra=model_id.is_some_and(|id|{let id=id.rsplit('/').next().unwrap_or(id).to_ascii_lowercase();let(base,variant)=id.rsplit_once(':').map_or((id.as_str(),None),|(base,variant)|(base,Some(variant)));matches!(base,"gpt-6-astra"|"gpt-6-astra-fast")&&variant.is_none_or(|v|matches!(v,"off"|"minimal"|"low"|"medium"|"high"|"xhigh"|"max"|"auto"))});if astra{SENPI_ASTRA_ULTRAWORK_DIRECTIVE}else{SENPI_ULTRAWORK_DIRECTIVE}}
pub fn select_directive_text(text:&str,directive:&str)->Option<String>{let clean=strip_astra_markers(text);let selected=directive.trim_end();let baseline=SENPI_ULTRAWORK_DIRECTIVE.trim_end();let astra=SENPI_ASTRA_ULTRAWORK_DIRECTIVE.trim_end();if !clean.contains(baseline)&&!clean.contains(astra){return None;}Some(clean.replace(baseline,selected).replace(astra,selected))}
fn strip_astra_markers(text:&str)->String{let mut out=String::with_capacity(text.len());for line in text.split_inclusive('\n'){if !(line.starts_with("<!-- omo-ultrawork-astra:")&&line.trim_end_matches(['\r','\n']).ends_with(" -->")){out.push_str(line);}}out}
