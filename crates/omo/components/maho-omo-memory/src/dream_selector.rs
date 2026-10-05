use std::{collections::BTreeSet, path::Path};
use memory_core::{journal::{cursor::{capture_cursor_snapshot, derive_state, initial_reflection_state, is_canonical_entry}, entries::TranscriptEntry, store::{JournalError, TranscriptJournal, TranscriptJournalOptions}}, search::{SearchDocument, SearchOptions, SearchToolCall, TranscriptConversation, TranscriptProvider, search_transcripts}};
pub use crate::dream_scoring::*;
pub struct DreamSelectorOptions<'a> { pub transcripts_dir: &'a Path, pub current_conversation_id: Option<&'a str>, pub auto_select_max: usize, pub auto_select_max_bytes: usize, pub now_ms: f64 }
struct LoadedConversation { conversation_id: String, entries: Vec<TranscriptEntry>, steps: usize, reflected_through: Option<String>, newest_message_id: String, last_activity: String, total_bytes: usize }
#[derive(Debug)]
pub enum DreamSelectorError { Io(std::io::Error), Journal(JournalError) }
impl std::fmt::Display for DreamSelectorError { fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { match self { Self::Io(error)=>error.fmt(f), Self::Journal(error)=>error.fmt(f) } } }
impl std::error::Error for DreamSelectorError {}
impl From<std::io::Error> for DreamSelectorError { fn from(error:std::io::Error)->Self {Self::Io(error)} }
impl From<JournalError> for DreamSelectorError { fn from(error:JournalError)->Self {Self::Journal(error)} }
fn activity(value:&str)->f64 { chrono::DateTime::parse_from_rfc3339(value).ok().map(|time| time.timestamp_millis().to_string().parse().unwrap_or(f64::NAN)).unwrap_or(f64::NAN) }
fn conversation_byte_length(entries:&[TranscriptEntry])->usize {
    entries.iter().map(|entry| match entry { TranscriptEntry::Text(entry)=>entry.text.clone(), TranscriptEntry::ToolCall(entry)=>[entry.args_text.as_deref(),entry.result_text.as_deref()].into_iter().flatten().collect::<Vec<_>>().join("\n") }).filter(|text| !text.is_empty()).collect::<Vec<_>>().join("\n").len()
}
fn load_conversations(dir:&Path)->Result<Vec<LoadedConversation>,DreamSelectorError> {
    let entries=match std::fs::read_dir(dir) {Ok(entries)=>entries,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(Vec::new()),Err(error)=>return Err(error.into())};
    let mut names=Vec::new(); for entry in entries { let entry=entry?; if entry.file_type()?.is_dir() {names.push(entry.file_name().to_string_lossy().into_owned());} } names.sort_by(|left,right|compare_conversation_ids(left,right));
    let mut loaded=Vec::new();
    for conversation_id in names {
        let journal_dir=dir.join(&conversation_id);
        let entries=TranscriptJournal::new(TranscriptJournalOptions::new(&journal_dir)).read_entries()?;
        let value=match std::fs::read(journal_dir.join("state.json")) {Ok(bytes)=>serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or(serde_json::Value::Null),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>serde_json::Value::Null,Err(error)=>return Err(error.into())};
        let mut state=initial_reflection_state();
        state.reflected_through_message_id=value.get("reflected_through_message_id").and_then(serde_json::Value::as_str).filter(|id|!id.is_empty()).map(str::to_owned);
        state.reflected_completed_steps=value.get("reflected_completed_steps").and_then(serde_json::Value::as_u64).and_then(|value|usize::try_from(value).ok()).unwrap_or(0);
        let state=derive_state(&state,&entries);
        let Some(snapshot)=capture_cursor_snapshot(&entries,&state) else {continue;};
        let Some(newest)=entries.iter().rev().find(|entry|is_canonical_entry(entry)) else {continue;};
        loaded.push(LoadedConversation {conversation_id, steps:state.steps_since_last_successful_reflection, reflected_through:state.reflected_through_message_id, newest_message_id:newest.source_message_id().to_owned(),last_activity:newest.captured_at().to_owned(),total_bytes:conversation_byte_length(&snapshot.entries),entries});
    }
    Ok(loaded)
}
fn search_documents(conversation:&LoadedConversation)->Vec<SearchDocument> {
    let mut grouped:Vec<(String,Vec<&TranscriptEntry>)>=Vec::new();
    for entry in &conversation.entries {if let Some((_,entries))=grouped.iter_mut().find(|(id,_)|id==entry.source_message_id()){entries.push(entry);}else{grouped.push((entry.source_message_id().to_owned(),vec![entry]));}}
    grouped.into_iter().map(|(id,entries)| {
        let mut content=Vec::new();let mut reasoning=Vec::new();let mut tools=Vec::new();let mut returns=Vec::new();
        for entry in &entries {match entry {TranscriptEntry::Text(text)=>{if text.kind=="reasoning"{reasoning.push(text.text.as_str());}else{content.push(text.text.as_str());}},TranscriptEntry::ToolCall(tool)=>{tools.push(SearchToolCall{name:tool.name.clone(),arguments:tool.args_text.clone()});if let Some(text)=&tool.result_text{returns.push(text.as_str());}}}}
        SearchDocument{id,conversation_id:conversation.conversation_id.clone(),date:entries.last().map(|entry|entry.captured_at().to_owned()),message_type:entries.first().map(|entry|entry.kind().to_owned()),content:Some(content.join("\n").into()),reasoning:Some(reasoning.join("\n")),summary:None,tool_calls:Some(tools),tool_return:Some(returns.join("\n").into()),func_response:None}
    }).collect()
}
struct OneConversation(TranscriptConversation);
impl TranscriptProvider for OneConversation {fn list_conversations(&self)->Vec<TranscriptConversation>{vec![self.0.clone()]}}
fn select_loaded(conversations:&[LoadedConversation],options:&DreamSelectorOptions<'_>,focus:Option<&str>)->DreamSelection {
    let target_bytes=options.auto_select_max_bytes.to_string().parse::<f64>().unwrap_or(0.0)/options.auto_select_max.to_string().parse::<f64>().unwrap_or(0.0);
    let candidates:Vec<_>=conversations.iter().map(|conversation| {
        let hits=focus.filter(|focus|!focus.trim().is_empty()).map(|focus|search_transcripts(&OneConversation(TranscriptConversation{id:conversation.conversation_id.clone(),hidden:None,messages:search_documents(conversation)}),focus,&SearchOptions{limit:Some(10),conversation_id:Some(conversation.conversation_id.clone()),..Default::default()}).len()).unwrap_or(0);
        let sources=conversation.entries.iter().map(TranscriptEntry::source_message_id).collect::<BTreeSet<_>>().len();
        let (operands,score)=score_dream_candidate(&DreamScoreInput{search_match_count:hits.to_string().parse().unwrap_or(0.0),steps_since_last_successful_reflection:conversation.steps.to_string().parse().unwrap_or(0.0),last_activity:&conversation.last_activity,distinct_source_count:sources.to_string().parse().unwrap_or(0.0),total_bytes:conversation.total_bytes.to_string().parse().unwrap_or(0.0),target_bytes,is_current:options.current_conversation_id==Some(conversation.conversation_id.as_str()),newest_message_covered:conversation.reflected_through.as_deref()==Some(conversation.newest_message_id.as_str()),now_ms:options.now_ms});
        DreamScoredConversation{conversation_id:conversation.conversation_id.clone(),total_bytes:conversation.total_bytes,last_activity_ms:activity(&conversation.last_activity),operands,score}
    }).collect();
    pack_dream_candidates(&rank_dream_candidates(&candidates),options.auto_select_max,options.auto_select_max_bytes)
}
pub fn compute_unreflected_volume(options:&DreamSelectorOptions<'_>)->Result<usize,DreamSelectorError>{Ok(load_conversations(options.transcripts_dir)?.iter().map(|conversation|conversation.total_bytes).sum())}
pub fn select_dream_conversations(options:&DreamSelectorOptions<'_>,focus:Option<&str>)->Result<DreamSelection,DreamSelectorError>{Ok(select_loaded(&load_conversations(options.transcripts_dir)?,options,focus))}
pub fn select_recent_dream_conversations(options:&DreamSelectorOptions<'_>,recent_n:usize,focus:Option<&str>)->Result<DreamSelection,DreamSelectorError>{let mut recent=load_conversations(options.transcripts_dir)?;recent.sort_by(|left,right|activity(&right.last_activity).total_cmp(&activity(&left.last_activity)).then_with(||compare_conversation_ids(&left.conversation_id,&right.conversation_id)));recent.truncate(recent_n);Ok(select_loaded(&recent,options,focus))}
#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::journal::entries::{TextTranscriptEntry,ToolCallTranscriptEntry};
    fn text(id:&str,kind:&str,value:&str,date:&str)->TranscriptEntry {TranscriptEntry::Text(TextTranscriptEntry::new(kind,value,date,format!("{id}:line"),format!("{id}:message")))}
    fn journal(dir:&Path,id:&str,entries:&[TranscriptEntry],state:serde_json::Value){let path=dir.join(id);std::fs::create_dir_all(&path).unwrap();let rows=entries.iter().map(|entry|serde_json::to_string(entry).unwrap()).collect::<Vec<_>>().join("\n");std::fs::write(path.join("transcript.jsonl"),format!("{rows}\n")).unwrap();std::fs::write(path.join("state.json"),serde_json::to_vec(&state).unwrap()).unwrap();}
    fn options(dir:&Path)->DreamSelectorOptions<'_>{DreamSelectorOptions{transcripts_dir:dir,current_conversation_id:None,auto_select_max:1,auto_select_max_bytes:10_000,now_ms:activity("2026-08-10T12:00:00.000Z")}}
    fn canonical_recency(kind:&str){let dir=tempfile::tempdir().unwrap();let tail=if kind=="tool_call"{TranscriptEntry::ToolCall(ToolCallTranscriptEntry::new(Some("read".into()),None,None,None,"2026-08-10T11:30:00.000Z","tail:line","tail:message"))}else{text("tail",kind,"context tail","2026-08-10T11:30:00.000Z")};journal(dir.path(),"stale-with-tail",&[text("stale","user","same","2026-08-10T09:00:00.000Z"),tail],serde_json::json!({}));journal(dir.path(),"actually-recent",&[text("recent","user","same","2026-08-10T10:00:00.000Z")],serde_json::json!({}));assert_eq!(select_recent_dream_conversations(&options(dir.path()),1,None).unwrap().conversation_ids,["actually-recent"]);assert_eq!(select_dream_conversations(&options(dir.path()),None).unwrap().conversation_ids,["actually-recent"]);}
    #[test]fn reasoning_tail_does_not_set_recency(){canonical_recency("reasoning");}
    #[test]fn error_tail_does_not_set_recency(){canonical_recency("error");}
    #[test]fn tool_tail_does_not_set_recency(){canonical_recency("tool_call");}
    #[test]fn utf8_volume_and_ranking_deterministic(){let dir=tempfile::tempdir().unwrap();let alpha=[text("alpha","user","focus 😀","2026-08-10T11:00:00.000Z"),TranscriptEntry::ToolCall(ToolCallTranscriptEntry::new(None,Some("x".into()),Some("結果".into()),None,"2026-08-10T11:30:00.000Z","alpha:tool","alpha:tool-message"))];journal(dir.path(),"alpha",&alpha,serde_json::json!({"steps_since_last_successful_reflection":20}));journal(dir.path(),"gamma",&[text("gamma","user","focus later","2026-08-10T09:00:00.000Z")],serde_json::json!({"steps_since_last_successful_reflection":10}));journal(dir.path(),"beta",&[text("beta","user","already reflected","2026-08-10T10:00:00.000Z")],serde_json::json!({"reflected_through_message_id":"beta:message"}));let options=DreamSelectorOptions{current_conversation_id:Some("gamma"),auto_select_max:5,..options(dir.path())};let bytes="focus 😀\nx\n結果".len()+"focus later".len();assert_eq!(compute_unreflected_volume(&options).unwrap(),bytes);let first=select_dream_conversations(&options,Some("focus")).unwrap();assert_eq!(first,DreamSelection{conversation_ids:vec!["gamma".into(),"alpha".into()],total_bytes:bytes});assert_eq!(select_dream_conversations(&options,Some("focus")).unwrap(),first);}
    #[test]fn missing_directory_is_empty(){let dir=tempfile::tempdir().unwrap();assert_eq!(compute_unreflected_volume(&options(&dir.path().join("absent"))).unwrap(),0);}
}
