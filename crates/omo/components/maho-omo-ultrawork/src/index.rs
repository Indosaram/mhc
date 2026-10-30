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
// skill-pointers/strip-quoted-regions.ts: the keyword arms only as a word of its own; quoted, injected,
// relayed and negated regions are blanked (offset-preserving) before the pattern runs.
const NOT_AFTER_IDENTIFIER:&str=r"(?<![A-Za-z0-9][-_.]|[/\\])";
const NOT_BEFORE_PATH:&str=r"(?![-_.]?[A-Za-z0-9]*[/\\])";
const NOT_INTO_IDENTIFIER:&str=r"(?!(?:--|[-_](?!(?:loop|plan|research|execute|mass)(?![-_.]?[A-Za-z0-9])))[A-Za-z0-9]|\.[A-Za-z0-9])";
fn fancy(pattern:&str)->fancy_regex::Regex{match fancy_regex::Regex::new(pattern){Ok(regex)=>regex,Err(error)=>panic!("invalid static ultrawork expression: {error}")}}
static ULTRAWORK_KEYWORD:std::sync::LazyLock<fancy_regex::Regex>=std::sync::LazyLock::new(||fancy(r"(?i)\b(?:ultrawork|ulw)\b"));
static ULTRAWORK_CURRENT_PROMPT_PATTERN:std::sync::LazyLock<fancy_regex::Regex>=std::sync::LazyLock::new(||{
    fancy(&format!(r"(?i){NOT_AFTER_IDENTIFIER}\b(?:ultrawork|ulw)\b{NOT_INTO_IDENTIFIER}{NOT_BEFORE_PATH}"))
});
static INJECTED_BLOCK:std::sync::LazyLock<fancy_regex::Regex>=std::sync::LazyLock::new(||fancy(r"(?i)<(omo-[a-z0-9-]+-pointer|ultrawork-mode|omo-ultrawork-reminder)>[\s\S]*?</\1>"));
static INLINE_CODE:std::sync::LazyLock<fancy_regex::Regex>=std::sync::LazyLock::new(||fancy(r"(?<!`)(`+)(?!`)[\s\S]*?(?<!`)\1(?!`)"));
static RELAY_HEADER:std::sync::LazyLock<fancy_regex::Regex>=std::sync::LazyLock::new(||fancy(r"^\s*(?:\[(?:[A-Z][A-Z0-9_-]{2,}|[\w.-]+ -> [\w.-]+)\]|[A-Z][\w.-]{0,30} \([^()\s]{1,40}\)(?:, | to )[\w.-]{1,40}:)"));
static BLOCK_QUOTE_LINE:std::sync::LazyLock<fancy_regex::Regex>=std::sync::LazyLock::new(||fancy(r"(?m)^ {0,3}>[^\n]*"));
static DOUBLE_QUOTED:std::sync::LazyLock<fancy_regex::Regex>=std::sync::LazyLock::new(||fancy("\"[^\"\\n]*\"|\u{201C}[^\u{201D}\\n]*\u{201D}"));
static URL_SPAN:std::sync::LazyLock<fancy_regex::Regex>=std::sync::LazyLock::new(||fancy(r#"(?i)(?<![a-z0-9+.-])[a-z][a-z0-9+.-]*://[^\s<>"'`]+"#));
static NEGATED_INVOCATION:std::sync::LazyLock<fancy_regex::Regex>=std::sync::LazyLock::new(||fancy("(?i)\\b(?:do\\s+not|don['\u{2019}]t|dont|never)\\s+(?:load|use|run|invoke|start|trigger|arm|enable)\\b[^.;:!?\\n]*?(?=[.;:!?\\n]|\\s(?:but|then|instead|however)\\b|\\s[\u{2014}\u{2013}-]\\s|$)"));
static FENCE_START:std::sync::LazyLock<fancy_regex::Regex>=std::sync::LazyLock::new(||fancy(r"(?m)^ {0,3}(`{3,}|~{3,})[^\n]*(?:\n|$)"));
/// Byte-length preserving, so match offsets stay valid against both `visible` and the original `text`.
fn mask(region:&str)->String{region.chars().map(|c|if matches!(c,'\r'|'\n'){c.to_string()}else{"\0".repeat(c.len_utf8())}).collect()}
fn mask_all(text:&str,regex:&fancy_regex::Regex)->String{
    let mut out=String::with_capacity(text.len());let mut last=0;
    for found in regex.find_iter(text).flatten(){out.push_str(&text[last..found.start()]);out.push_str(&mask(found.as_str()));last=found.end();}
    out.push_str(&text[last..]);out
}
/// `stripQuotedRegions`: quoted spans plus negated invocations masked.
fn strip_quoted_regions(text:&str)->String{mask_all(&strip_quoted_spans(text),&NEGATED_INVOCATION)}
/// `stripQuotedSpans`: injected blocks, fences, inline code, block quotes, quotes, URLs and relays masked.
fn strip_quoted_spans(text:&str)->String{
    if RELAY_HEADER.is_match(text).unwrap_or(false){return mask(text);}
    let mut visible=mask_all(text,&INJECTED_BLOCK);
    let mut from=0;
    while let Ok(Some(opening))=FENCE_START.captures_from_pos(&visible,from){
        let (Some(whole),Some(fence))=(opening.get(0),opening.get(1)) else {break};
        let fence=fence.as_str();let fence_char=&fence[..1];
        let end_pattern=fancy(&format!(r"(?m)^ {{0,3}}{}{{{},}}[ \t]*\r?$",fancy_regex::escape(fence_char),fence.len()));
        let end=match end_pattern.find_from_pos(&visible,whole.end()){Ok(Some(closing))=>closing.end(),_=>visible.len()};
        let start=whole.start();
        visible=format!("{}{}{}",&visible[..start],mask(&visible[start..end]),&visible[end..]);
        from=end;
    }
    let visible=mask_all(&visible,&INLINE_CODE);let visible=mask_all(&visible,&BLOCK_QUOTE_LINE);
    let visible=mask_all(&visible,&DOUBLE_QUOTED);mask_all(&visible,&URL_SPAN)
}
/// Every visible keyword invocation as (start, end, is_full_word) byte offsets into `text`.
fn visible_invocations(text:&str)->Vec<(usize,usize,bool)>{
    if cannot_contain_keyword(text){return Vec::new();}
    let visible=strip_quoted_regions(text);
    ULTRAWORK_CURRENT_PROMPT_PATTERN.find_iter(&visible).flatten()
        .filter(|found|!visible[found.end()..].starts_with("--"))
        .map(|found|(found.start(),found.end(),!found.as_str().eq_ignore_ascii_case("ulw"))).collect()
}
/// Fast negative for the keyword pattern: pure-ASCII text with neither `ulw` nor `ultrawork` (any case)
/// cannot match `(?i)\b(?:ultrawork|ulw)\b` after masking, which only removes characters. Non-ASCII text
/// takes the full path because Unicode case folding (e.g. the Kelvin sign) can match ASCII letters.
fn cannot_contain_keyword(text:&str)->bool{
    let has=|needle:&[u8]|text.as_bytes().windows(needle.len()).any(|window|window.eq_ignore_ascii_case(needle));
    text.is_ascii()&&!has(b"ulw")&&!has(b"ultrawork")
}
pub fn already_embedded(text:&str)->bool{text.contains("<ultrawork-mode>")&&text.contains("</ultrawork-mode>")}

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]pub struct ArmingSnapshot{pub was_armed:bool,pub compact_rearm_pending:bool}
#[derive(Clone,Debug,PartialEq,Eq)]pub struct UltraworkClassification{pub matched_ulw:bool,pub matched_ultrawork:bool,pub occurrence_count:usize,pub effective:bool,pub stage:&'static str,pub route:&'static str,pub suppression_reason:&'static str}
pub fn arming_snapshot(id:Option<&str>)->ArmingSnapshot{let a=shared_session_arming();let a=a.lock().unwrap_or_else(std::sync::PoisonError::into_inner);ArmingSnapshot{was_armed:a.is_armed(id),compact_rearm_pending:a.is_compact_rearm_pending(id)}}
pub fn classify_ultrawork_input(text:&str,source:InputSource,snapshot:ArmingSnapshot)->UltraworkClassification{
    let visible=visible_invocations(text);let mut result=UltraworkClassification{matched_ulw:visible.iter().any(|(_,_,full)|!*full),matched_ultrawork:visible.iter().any(|(_,_,full)|*full),occurrence_count:visible.len(),effective:false,stage:"none",route:"none",suppression_reason:"none"};if source==InputSource::Extension{result.suppression_reason="extension_source";return result;}
    if visible.is_empty(){result.suppression_reason=if cannot_contain_keyword(text){"no_keyword"}else if ULTRAWORK_KEYWORD.is_match(&strip_quoted_regions(text)).unwrap_or(false){"identifier_reference"}else if ULTRAWORK_KEYWORD.is_match(&strip_quoted_spans(text)).unwrap_or(false){"negated_mention"}else{"no_keyword"};return result;}
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
pub fn select_directive_text(text:&str,directive:&str)->Option<String>{let baseline=SENPI_ULTRAWORK_DIRECTIVE.trim_end();let astra=SENPI_ASTRA_ULTRAWORK_DIRECTIVE.trim_end();if !text.contains("<!-- omo-ultrawork-astra:")&&!text.contains(baseline)&&!text.contains(astra){return None;}let clean=strip_astra_markers(text);let selected=directive.trim_end();if !clean.contains(baseline)&&!clean.contains(astra){return None;}Some(clean.replace(baseline,selected).replace(astra,selected))}
fn strip_astra_markers(text:&str)->String{let mut out=String::with_capacity(text.len());for line in text.split_inclusive('\n'){if !(line.starts_with("<!-- omo-ultrawork-astra:")&&line.trim_end_matches(['\r','\n']).ends_with(" -->")){out.push_str(line);}}out}
