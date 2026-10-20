use std::{cell::{Cell,RefCell},collections::BTreeMap,rc::Rc};
use serde::{Serialize,Deserialize};
use super::types::*;
pub use super::types::SectionSeed;

#[derive(Clone)]
pub struct SystemSection {pub key:String,pub render:Rc<dyn Fn(&JsonValue)->Result<String>>}
pub fn define_system_section(key:String,render:Rc<dyn Fn(&JsonValue)->Result<String>>)->SystemSection{SystemSection{key,render}}
pub fn section_seed(section:&SystemSection,value:JsonValue)->SectionSeed{SectionSeed::Set{key:section.key.clone(),value}}
pub fn remove_section(key:String)->SectionSeed{SectionSeed::Remove{key,remove:True}}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(tag="action",rename_all="lowercase")]
pub enum SectionRecord {Set{key:String,value:JsonValue,rendered:String},Remove{key:String}}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct SystemEntryData {#[serde(default,skip_serializing_if="Option::is_none")]pub baseline:Option<True>,pub sections:Vec<SectionRecord>}
#[derive(Clone,Debug,PartialEq)]
pub struct SectionState {pub value:JsonValue,pub rendered:String}
pub type Canonical=Vec<(String,SectionState)>;
pub struct CanonicalFold {pub canonical:Canonical,pub newest_managed:Option<Id>,pub newest_baseline:Option<Id>}
pub async fn fold_canonical(tx:&dyn TxReads,conversation_id:Id)->Result<CanonicalFold>{
    let mut managed=Vec::new();let mut before=None;let mut baseline=None;
    'scan:loop{
        let page=tx.scan_entries(EntryScan{conversation_id,kind:Some("pi.system".into()),with_head:None,before,limit:64}).await?;
        let length=page.len();let last=page.last().map(|entry|entry.id);
        for entry in page{
            let data:SystemEntryData=from_object(entry.data.as_ref().ok_or_else(||Error::InvalidShape("managed entry missing data".into()))?)?;
            let is_baseline=data.baseline.is_some();managed.push((entry.id,data));
            if is_baseline{baseline=Some(entry.id);break 'scan;}
        }
        if length<64{break;}before=last;
    }
    managed.reverse();let newest_managed=managed.last().map(|(id,_)|*id);let mut canonical=Vec::new();
    for(_,data)in managed{for record in data.sections{match record{SectionRecord::Set{key,value,rendered}=>ordered_set(&mut canonical,key,SectionState{value,rendered}),SectionRecord::Remove{key}=>canonical.retain(|(k,_)|k!=&key)}}}
    Ok(CanonicalFold{canonical,newest_managed,newest_baseline:baseline})
}
fn ordered_set<T>(values:&mut Vec<(String,T)>,key:String,value:T){if let Some((_,target))=values.iter_mut().find(|(k,_)|k==&key){*target=value;}else{values.push((key,value));}}
type Wrapper=Rc<dyn Fn(String)->Result<String>>;
#[derive(Clone)]
pub struct SystemSectionDraft {values:Vec<(String,JsonValue)>,wrappers:BTreeMap<String,Vec<Wrapper>>,touched:std::collections::BTreeSet<String>}
impl SystemSectionDraft{
    pub fn new(canonical:&Canonical)->Self{Self{values:canonical.iter().map(|(k,v)|(k.clone(),v.value.clone())).collect(),wrappers:BTreeMap::new(),touched:std::collections::BTreeSet::new()}}
    pub fn get(&self,section:&SystemSection)->Option<JsonValue>{self.values.iter().find(|(key,_)|key==&section.key).map(|(_,value)|value.clone())}
    pub fn set(&mut self,section:&SystemSection,value:JsonValue){self.set_key(section.key.clone(),value);}
    fn set_key(&mut self,key:String,value:JsonValue){self.touched.insert(key.clone());ordered_set(&mut self.values,key,value);}
    pub fn delete(&mut self,key:&str){self.values.retain(|(k,_)|k!=key);self.wrappers.remove(key);self.touched.insert(key.into());}
    pub fn wrap(&mut self,section:&SystemSection,wrapper:Wrapper){self.wrappers.entry(section.key.clone()).or_default().push(wrapper);self.touched.insert(section.key.clone());}
    pub fn freeze(&self,canonical:&Canonical,registry:&BTreeMap<String,SystemSection>)->Result<Canonical>{
        let mut out=Vec::new();for(key,value)in &self.values{
            if !self.touched.contains(key){if let Some((_,previous))=canonical.iter().find(|(k,_)|k==key){out.push((key.clone(),previous.clone()));}continue;}
            let section=registry.get(key).ok_or_else(||Error::InvalidShape(format!("unknown system section {key}")))?;
            let mut rendered=(section.render)(value)?;for wrapper in self.wrappers.get(key).into_iter().flatten(){rendered=wrapper(rendered)?;}
            out.push((key.clone(),SectionState{value:value.clone(),rendered}));
        }Ok(out)
    }
}
#[derive(Default)]
pub struct SectionRegistry {pub map:RefCell<BTreeMap<String,SystemSection>>,pub revision:Cell<u64>}
impl SectionRegistry{
    pub fn register(&self,section:SystemSection)->Result<()>{if self.map.borrow().contains_key(&section.key){return Err(Error::InvalidShape(format!("duplicate section {}",section.key)));}self.map.borrow_mut().insert(section.key.clone(),section);self.revision.set(self.revision.get()+1);Ok(())}
    pub fn unregister(&self,section:&SystemSection){let remove=self.map.borrow().get(&section.key).is_some_and(|current|Rc::ptr_eq(&current.render,&section.render));if remove{self.map.borrow_mut().remove(&section.key);self.revision.set(self.revision.get()+1);}}
}
#[derive(Default)]
pub struct ToolRegistry {pub map:RefCell<BTreeMap<String,Rc<dyn AnyToolDeclaration>>>,pub revision:Cell<u64>}
impl ToolRegistry{
    pub fn register(&self,tool:Rc<dyn AnyToolDeclaration>)->Result<()>{let name=tool.name().to_owned();if self.map.borrow().contains_key(&name){return Err(Error::InvalidShape(format!("duplicate tool {name}")));}self.map.borrow_mut().insert(name,tool);self.revision.set(self.revision.get()+1);Ok(())}
    pub fn unregister(&self,tool:&Rc<dyn AnyToolDeclaration>){let remove=self.map.borrow().get(tool.name()).is_some_and(|current|Rc::ptr_eq(current,tool));if remove{self.map.borrow_mut().remove(tool.name());self.revision.set(self.revision.get()+1);}}
}
pub fn system_sections()->BTreeMap<String,SystemSection>{
    let identity=define_system_section("identity".into(),Rc::new(|value|match value{JsonValue::String(value)=>value.as_str().map(str::to_owned).ok_or_else(||Error::InvalidShape("identity contains unpaired surrogate".into())),_=>Err(Error::InvalidShape("identity must be text".into()))}));
    let environment=define_system_section("environment".into(),Rc::new(|value|{let value:JsonObject=from_json(value)?;match value.get("cwd"){Some(JsonValue::String(cwd))=>Ok(format!("Working directory: {}",cwd.as_str().ok_or_else(||Error::InvalidShape("cwd contains unpaired surrogate".into()))?)),_=>Err(Error::InvalidShape("environment cwd missing".into()))}}));
    let skills=define_system_section("skills".into(),Rc::new(|value|{#[derive(Deserialize)]struct Skill{name:String,description:String}let skills:Vec<Skill>=from_json(value)?;Ok(skills.iter().map(|skill|format!("- {}: {}",skill.name,skill.description)).collect::<Vec<_>>().join("\n"))}));
    [identity,environment,skills].into_iter().map(|section|(section.key.clone(),section)).collect()
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct PreparationSettings {#[serde(default,skip_serializing_if="Option::is_none")]pub model:Option<ModelRef>,pub thinking_level:ThinkingLevel,pub selected_tools:Vec<String>,pub profile:String}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct PreparationSnapshot {pub newest_managed:Option<Id>,pub newest_baseline:Option<Id>,pub newest_head:Option<Id>,pub settings:PreparationSettings,pub sections_rev:u64,pub tools_rev:u64}
pub fn same_snapshot(a:&PreparationSnapshot,b:&PreparationSnapshot)->Result<bool>{Ok(serde_json::to_string(a)?==serde_json::to_string(b)?)}
pub struct Snapshot {pub snapshot:PreparationSnapshot,pub canonical:Canonical,pub seed:Option<Vec<SectionSeed>>}
pub async fn take_snapshot(tx:&dyn TxReads,conversation_id:Id,sections:&SectionRegistry,tools:&ToolRegistry)->Result<Snapshot>{
    let folded=fold_canonical(tx,conversation_id).await?;
    let head=tx.newest_entry(EntryScan{conversation_id,kind:None,with_head:Some(true),before:None,limit:1}).await?;
    let DocumentSnapshot::Rewindable(state)=tx.snapshot(DocRef::Rewindable{conversation_id})?else{return Err(Error::InvalidShape("expected rewindable snapshot".into()))};
    let conversation=tx.conversation(conversation_id).await?;
    let seed=if folded.newest_managed.is_none(){conversation.and_then(|c|c.sections).map(|s|s.into_iter().map(|s|match s{super::types::SectionSeed::Set{key,value}=>SectionSeed::Set{key,value},super::types::SectionSeed::Remove{key,remove}=>SectionSeed::Remove{key,remove}}).collect())}else{None};
    Ok(Snapshot{snapshot:PreparationSnapshot{newest_managed:folded.newest_managed,newest_baseline:folded.newest_baseline,newest_head:head.map(|entry|entry.id),settings:PreparationSettings{model:state.model,thinking_level:state.thinking_level,selected_tools:state.selected_tools,profile:state.profile},sections_rev:sections.revision.get(),tools_rev:tools.revision.get()},canonical:folded.canonical,seed})
}
pub struct HookOutput {pub tools:Option<Vec<Rc<dyn AnyToolDeclaration>>>}
pub type SystemInstructionsHandler=Rc<dyn for<'a> Fn(&'a mut SystemSectionDraft,&'a PreparationSettings,&'a [Rc<dyn AnyToolDeclaration>],&'a HookInfo,&'a Context)->LocalBoxFuture<'a,Result<Option<HookOutput>>>>;
pub struct SystemInstructionsHook {pub handler:SystemInstructionsHandler,pub info:HookInfo}
pub struct PreparedDraft {pub desired:Canonical,pub tools:Vec<Rc<dyn AnyToolDeclaration>>}
pub async fn prepare_draft(hooks:&[SystemInstructionsHook],sections:&SectionRegistry,tools_registry:&ToolRegistry,canonical:&Canonical,seed:Option<&[SectionSeed]>,settings:&PreparationSettings,on_report:&dyn Fn(String),ctx:&Context)->Result<PreparedDraft>{
    let mut draft=SystemSectionDraft::new(canonical);
    for seed in seed.unwrap_or_default(){match seed{SectionSeed::Set{key,value}=>draft.set_key(key.clone(),value.clone()),SectionSeed::Remove{key,..}=>draft.delete(key)}}
    let mut defaults=Vec::new();for name in &settings.selected_tools{match tools_registry.map.borrow().get(name){Some(tool)=>defaults.push(tool.clone()),None=>on_report(format!("selected tool {name} is not registered"))}}
    let mut tools=defaults.clone();
    for hook in hooks{let before=draft.clone();match (hook.handler)(&mut draft,settings,&defaults,&hook.info,ctx).await{
        Ok(Some(HookOutput{tools:Some(override_tools)}))=>tools=override_tools,
        Ok(Some(HookOutput{tools:None}))|Ok(None)=>{},
        Err(error)=>{if ctx.is_aborted(){return Err(error);}draft=before;on_report(error.to_string());}
    }}
    let desired=draft.freeze(canonical,&sections.map.borrow())?;Ok(PreparedDraft{desired,tools})
}
#[derive(Clone,Debug)]
pub struct ManagedEntryPlan {pub data:SystemEntryData,pub model:Vec<StoredMessage>,pub edits:Option<Vec<ContextEdit>>}
pub async fn plan_managed_entry(tx:&dyn TxReads,conversation_id:Id,snapshot:&PreparationSnapshot,canonical:&Canonical,desired:&Canonical,tools:&[Rc<dyn AnyToolDeclaration>],now:u64)->Result<Option<ManagedEntryPlan>>{
    let need_baseline=snapshot.newest_managed.is_none()||snapshot.newest_head.is_some_and(|head|snapshot.newest_baseline.is_none_or(|baseline|head>baseline));
    let context=tx.context(conversation_id,None).await?;let previous=effective_tools(&context.messages)?;
    let tool_of=|tool:&Rc<dyn AnyToolDeclaration>|SystemTool{name:tool.name().into(),description:tool.description().into(),parameters:tool.parameters().clone()};
    if need_baseline{
        let sections:Vec<_>=desired.iter().map(|(key,state)|SectionRecord::Set{key:key.clone(),value:state.value.clone(),rendered:state.rendered.clone()}).collect();
        let content=desired.iter().map(|(key,state)|format!("## {key}\n{}",state.rendered)).collect::<Vec<_>>().join("\n\n");
        let edits:Vec<_>=context.entries.iter().filter(|entry|entry.kind=="pi.system"&&entry.id>snapshot.newest_head.unwrap_or(0)).map(|entry|ContextEdit::Omit{target:entry.id}).collect();
        let message=to_stored(&Message::System(SystemMessage{content,tools_added:Some(tools.iter().map(tool_of).collect()),tools_removed:None,timestamp:i64::try_from(now).map_err(|_|Error::InvalidShape("timestamp overflow".into()))?}))?;
        return Ok(Some(ManagedEntryPlan{data:SystemEntryData{baseline:Some(True),sections},model:vec![message],edits:if edits.is_empty(){None}else{Some(edits)}}));
    }
    let mut changed=Vec::new();
    for(key,state)in desired{if canonical.iter().find(|(k,_)|k==key).is_none_or(|(_,previous)|previous.rendered!=state.rendered||previous.value!=state.value){changed.push(SectionRecord::Set{key:key.clone(),value:state.value.clone(),rendered:state.rendered.clone()});}}
    for(key,_)in canonical{if !desired.iter().any(|(k,_)|k==key){changed.push(SectionRecord::Remove{key:key.clone()});}}
    let added:Vec<_>=tools.iter().filter(|tool|!previous.iter().any(|previous|previous.name==tool.name())).map(tool_of).collect();
    let removed:Vec<_>=previous.into_iter().filter(|previous|!tools.iter().any(|tool|tool.name()==previous.name)).map(|tool|ToolReference{name:tool.name}).collect();
    if changed.is_empty()&&added.is_empty()&&removed.is_empty(){return Ok(None);}
    let render_changed=changed.iter().any(|record|match record{SectionRecord::Remove{..}=>true,SectionRecord::Set{key,rendered,..}=>canonical.iter().find(|(k,_)|k==key).is_none_or(|(_,state)|state.rendered!=*rendered)});
    if !render_changed&&added.is_empty()&&removed.is_empty(){return Ok(Some(ManagedEntryPlan{data:SystemEntryData{baseline:None,sections:changed},model:Vec::new(),edits:None}));}
    let content=changed.iter().map(|record|match record{SectionRecord::Set{key,rendered,..}=>format!("The {key} section now reads:\n{rendered}"),SectionRecord::Remove{key}=>format!("The {key} section no longer applies.")}).collect::<Vec<_>>().join("\n\n");
    let message=to_stored(&Message::System(SystemMessage{content,tools_added:if added.is_empty(){None}else{Some(added)},tools_removed:if removed.is_empty(){None}else{Some(removed)},timestamp:i64::try_from(now).map_err(|_|Error::InvalidShape("timestamp overflow".into()))?}))?;
    Ok(Some(ManagedEntryPlan{data:SystemEntryData{baseline:None,sections:changed},model:vec![message],edits:None}))
}
pub fn effective_tools(messages:&[StoredMessage])->Result<Vec<SystemTool>>{
    let mut tools=Vec::new();for message in messages{
        let object:JsonObject=from_json(message.as_json())?;
        if !matches!(object.get("role"),Some(JsonValue::String(role)) if role.as_str()==Some("system")){continue;}
        let system:SystemMessage=from_json(message.as_json())?;
        for removed in system.tools_removed.unwrap_or_default(){tools.retain(|(name,_):&(String,SystemTool)|name!=&removed.name);}
        for tool in system.tools_added.unwrap_or_default(){ordered_set(&mut tools,tool.name.clone(),tool);}
    }Ok(tools.into_iter().map(|(_,tool)|tool).collect())
}
fn from_json<T:serde::de::DeserializeOwned>(value:&JsonValue)->Result<T>{Ok(serde_json::from_str(&serde_json::to_string(value)?)?)}
fn from_object<T:serde::de::DeserializeOwned>(value:&JsonObject)->Result<T>{Ok(serde_json::from_str(&serde_json::to_string(value)?)?)}
pub async fn commit_prepared(tx:&mut dyn CoreTx,conversation_id:Id,expected:&PreparationSnapshot,sections:&SectionRegistry,tools:&ToolRegistry,desired:&Canonical,loadout:&[Rc<dyn AnyToolDeclaration>],now:u64)->Result<Transition>{
    let current=take_snapshot(tx,conversation_id,sections,tools).await?;
    if !same_snapshot(expected,&current.snapshot)?{return Ok(Transition::Retry);}
    if let Some(plan)=plan_managed_entry(tx,conversation_id,&current.snapshot,&current.canonical,desired,loadout,now).await?{
        let data:JsonObject=serde_json::from_str(&serde_json::to_string(&plan.data)?)?;
        tx.append_entry(conversation_id,NewEntry{kind:"pi.system".into(),model:Some(plan.model),data:Some(data),head:None,edits:plan.edits})?;
    }Ok(Transition::Checkpoint(Checkpoint{phase:"prepared".into(),fields:JsonObject::new()}))
}

#[cfg(test)]
#[path="system_tests.rs"]
mod tests;
