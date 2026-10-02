use std::collections::{BTreeMap, BTreeSet};
use maho_core::compaction::{compaction::estimate_tokens,utils::extract_patched_paths};
use serde_json::{Value,json};
pub const POST_COMPACT_RESTORATION_CUSTOM_TYPE: &str = "compaction.post-compact-restoration";
#[derive(Clone, Debug, PartialEq)]
pub struct RestorationItem {pub content:String,pub label:String,pub priority:u32,pub tokens:u64,pub kind:&'static str}
#[derive(Clone, Debug, Default)]
pub struct RestorationSettings {pub max_items:Option<f64>,pub max_tokens_per_item:Option<f64>,pub max_total_tokens:Option<f64>,pub context_ratio:Option<f64>}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RestorationTrackerState {pub items:BTreeMap<String,RestorationItem>,pub restored_labels:BTreeSet<String>,pub pending_payload:Option<Value>}
pub struct PreparePendingPayloadOptions<'a> {pub accepted:bool,pub reason:&'a str,pub compaction_entry_id:&'a str,pub context_window:f64,pub usage_tokens:Option<f64>,pub reserve_tokens:f64,pub settings:&'a RestorationSettings,pub kept_messages:&'a [Value]}
fn normalize(value:Option<f64>,fallback:f64)->f64 {value.filter(|v|v.is_finite() && *v>=0.0).unwrap_or(fallback)}
fn tokens(text:&str)->u64 {estimate_tokens(&json!({"role":"custom","customType":"restoration","content":text,"display":false,"timestamp":0}))}
fn track_file(state:&mut RestorationTrackerState,path:&str,operation:&str) {
    let mut operations=BTreeSet::new();
    if let Some(existing)=state.items.get(path) && let Some((_,tail))=existing.content.rsplit_once('(') && let Some((body,_))=tail.rsplit_once(')') {
        for op in body.split(',').map(str::trim).filter(|op|matches!(*op,"read"|"write"|"edit")) {operations.insert(op.to_owned());}
    }
    operations.insert(operation.into());
    let ordered:Vec<_>=["read","write","edit"].into_iter().filter(|op|operations.contains(*op)).collect();
    let content=format!("{path} ({})",ordered.join(", "));
    state.items.insert(path.into(),RestorationItem {tokens:tokens(&content),content,label:path.into(),priority:if operation=="read" && operations.len()==1 {50} else {100},kind:"file"});
}
pub fn track_tool_call(state:&mut RestorationTrackerState,name:&str,input:&Value) {
    match name {
        "read"|"write"|"edit" => {if let Some(path)=input.get("path").and_then(Value::as_str).filter(|p|!p.is_empty()) {track_file(state,path,name);}}
        "apply_patch" => {if let Some(patch)=input.get("input").and_then(Value::as_str) {for path in extract_patched_paths(patch).into_iter().filter(|p|!p.is_empty()) {track_file(state,&path,"edit");}}}
        "skill"|"load_skill" => {
            if let Some(skill)=input.get("name").and_then(Value::as_str).or_else(||input.get("skillName").and_then(Value::as_str)).filter(|s|!s.is_empty()) {
                state.items.insert(skill.into(),RestorationItem {content:skill.into(),label:skill.into(),priority:80,tokens:tokens(skill),kind:"skill"});
            }
        }
        _ => {}
    }
}
pub fn compute_restoration_budget(options:&PreparePendingPayloadOptions<'_>)->f64 {
    let window=options.context_window.max(0.0);
    normalize(options.settings.max_total_tokens,50000.).floor().min((window*normalize(options.settings.context_ratio,0.15)).floor()).min((window-options.usage_tokens.unwrap_or(0.0).max(0.0)-options.reserve_tokens).max(0.0)).floor()
}
fn message_text(message:&Value)->String {
    if let Some(content)=message.get("content") {
        if let Some(text)=content.as_str() {return text.into();}
        if let Some(blocks)=content.as_array() {return blocks.iter().filter(|b|b.get("type").and_then(Value::as_str)==Some("text")).filter_map(|b|b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n");}
    }
    if let Some(summary)=message.get("summary").and_then(Value::as_str) {return summary.into();}
    if let Some(command)=message.get("command").and_then(Value::as_str) {return format!("{command}\n{}",message.get("output").and_then(Value::as_str).unwrap_or_default());}
    String::new()
}
fn truncate_item(item:&RestorationItem,max:u64)->RestorationItem {
    if item.tokens<=max {return item.clone();}
    let notice="[... truncated]";
    let target=max.saturating_sub(tokens(notice)+1);
    let units:Vec<_>=item.content.encode_utf16().collect();
    let (mut low,mut high)=(0,units.len());
    if target>0 {while low<high {let mid=(low+high).div_ceil(2);if tokens(&String::from_utf16_lossy(&units[..mid]))<=target {low=mid;} else {high=mid-1;}}}
    let body=String::from_utf16_lossy(&units[..low]);
    let body=body.trim_end();
    let content=if body.is_empty() {notice.into()} else {format!("{body}\n{notice}")};
    RestorationItem {tokens:tokens(&content),content,..item.clone()}
}
fn escape_attribute(value:&str)->String {value.replace('&',"&amp;").replace('"',"&quot;").replace('<',"&lt;").replace('>',"&gt;")}
pub fn prepare_pending_payload(state:&mut RestorationTrackerState,options:&PreparePendingPayloadOptions<'_>) {
    if !options.accepted {return;}
    let budget=compute_restoration_budget(options);
    if budget<=0.0 {state.pending_payload=None;return;}
    if !options.kept_messages.is_empty() {let kept=options.kept_messages.iter().map(message_text).collect::<Vec<_>>().join("\n");state.items.retain(|label,_|!kept.contains(label.as_str()));}
    let max=normalize(options.settings.max_tokens_per_item,5000.).floor() as u64;
    let mut candidates:Vec<_>=state.items.values().filter(|i|!state.restored_labels.contains(&i.label)).map(|i|truncate_item(i,max)).collect();
    let collator = icu_collator::Collator::try_new(Default::default(), Default::default()).expect("compiled collation data is available");
    candidates.sort_by(|a,b|b.priority.cmp(&a.priority).then(a.tokens.cmp(&b.tokens)).then_with(||collator.compare(&a.label, &b.label)));
    let mut selected=Vec::new();let mut total=0;
    for item in candidates {
        if selected.len()>=normalize(options.settings.max_items,10.).floor() as usize {break;}
        if (total+item.tokens) as f64>budget {continue;}
        total+=item.tokens;
        selected.push(item);
    }
    if selected.is_empty() {state.pending_payload=None;return;}
    let mut lines=vec!["[Restored context after compaction — files and skills from before compaction]".into(),format!("reason: {}",options.reason),format!("compactionEntryId: {}",options.compaction_entry_id),String::new()];
    for (kind,plural) in [("file","files"),("skill","skills")] {
        let items:Vec<_>=selected.iter().filter(|i|i.kind==kind).collect();if items.is_empty() {continue;}
        lines.push(format!("<restored-{plural}>"));for item in items {lines.push(format!("<{kind} label=\"{}\">{}</{kind}>",escape_attribute(&item.label),item.content));}lines.push(format!("</restored-{plural}>"));lines.push(String::new());
    }
    let items:Vec<_>=selected.iter().map(|i|json!({"content":i.content,"label":i.label,"priority":i.priority,"tokens":i.tokens,"type":i.kind})).collect();
    state.pending_payload=Some(json!({"customType":POST_COMPACT_RESTORATION_CUSTOM_TYPE,"content":lines.join("\n"),"display":false,"details":{"schema":"senpi.compaction.post-compact-restoration.v1","budgetTokens":budget,"compactionEntryId":options.compaction_entry_id,"reason":options.reason,"items":items}}));
}
pub fn consume_pending_payload(state:&mut RestorationTrackerState)->Option<Value> {
    let payload=state.pending_payload.take()?;
    if let Some(items)=payload["details"]["items"].as_array() {for label in items.iter().filter_map(|i|i.get("label").and_then(Value::as_str)) {state.restored_labels.insert(label.into());}}
    Some(payload)
}
