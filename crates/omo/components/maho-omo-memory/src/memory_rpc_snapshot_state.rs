use std::collections::BTreeMap;
use memory_core::git::{GitMemoryRepo,errors::GitError};
use serde::Serialize;
use crate::context::MemoryIdentityContext;
#[derive(Debug,Serialize)]
#[serde(rename_all="camelCase")]
pub struct MemoryRpcRepoState{
    #[serde(skip_serializing_if="Option::is_none")]pub head_sha:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub head_subject:Option<String>,
    #[serde(rename="committedAtISO",skip_serializing_if="Option::is_none")]pub committed_at_iso:Option<String>,
    pub dirty:bool,pub dirty_paths:usize,pub system_tokens_estimate:usize,
    #[serde(skip_serializing_if="Option::is_none")]pub total_bytes:Option<u64>,
    #[serde(skip_serializing_if="Option::is_none")]pub system_bytes:Option<u64>,
    #[serde(skip_serializing_if="Option::is_none")]pub file_count:Option<u64>,
    #[serde(skip_serializing_if="Option::is_none")]pub entries_today:Option<u64>,
    #[serde(rename="previousEntryAtISO",skip_serializing_if="Option::is_none")]pub previous_entry_at_iso:Option<String>,
}
/// Whole-tree size numbers for one commit (pin `memoryTreeStats`).
#[derive(Debug,Clone,Default,PartialEq,Eq)]
pub struct MemoryTreeStats{pub total_bytes:u64,pub file_count:u64,pub system_bytes:u64,
    /// `system/**/*.md` only: the compiled-memory surface behind `systemTokensEstimate`.
    pub system_markdown_bytes:u64,pub by_top_level:BTreeMap<String,u64>}
pub fn memory_tree_stats(entries:&[memory_core::git::GitTreeSizedEntry])->MemoryTreeStats{
    let mut stats=MemoryTreeStats{file_count:entries.len() as u64,..Default::default()};
    for entry in entries{
        stats.total_bytes+=entry.bytes;
        if entry.path.starts_with("system/"){stats.system_bytes+=entry.bytes;}
        if entry.path.starts_with("system/")&&entry.path.ends_with(".md"){stats.system_markdown_bytes+=entry.bytes;}
        let top=entry.path.split_once('/').map_or(entry.path.as_str(),|(top,_)|top);
        *stats.by_top_level.entry(top.to_owned()).or_default()+=entry.bytes;
    }
    stats
}
#[derive(Default,Debug,Serialize,PartialEq)]
#[serde(rename_all="camelCase")]
pub struct MemoryRpcJournalState{pub backlog_steps:f64,pub pending_compaction:bool,pub total_steps:f64,pub reflected_steps:f64}
fn count(value:Option<&serde_json::Value>)->f64{value.and_then(serde_json::Value::as_f64).filter(|number|number.is_finite()&&*number>0.0).map(f64::floor).unwrap_or(0.0)}
pub fn read_memory_rpc_journal_state(context:&MemoryIdentityContext,session:&str)->MemoryRpcJournalState{
    let empty=MemoryRpcJournalState{pending_compaction:context.ledger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending_compaction,..Default::default()};
    let Some(value)=std::fs::read(context.identity_paths.transcripts.join(session).join("state.json")).ok().and_then(|bytes|serde_json::from_slice::<serde_json::Value>(&bytes).ok())else{return empty;};
    let Some(state)=value.as_object()else{return empty;};
    MemoryRpcJournalState{backlog_steps:count(state.get("steps_since_last_successful_reflection")),pending_compaction:state.get("pending_compaction")==Some(&serde_json::Value::Bool(true))||empty.pending_compaction,total_steps:count(state.get("total_completed_steps")),reflected_steps:count(state.get("reflected_completed_steps"))}
}
/// Pin `readMemoryRpcRepoState`: one sized-tree walk per commit feeds both the token estimate and the size
/// fields; a repeat read on the same HEAD reuses the cached stats, and a failed walk omits every size field
/// rather than reporting a bogus zero. The timeline fields (`entriesToday`, `previousEntryAtISO`) are read
/// alongside (pin `readMemoryRpcTimeline`).
pub fn read_memory_rpc_repo_state(repo:&GitMemoryRepo,cache:&mut BTreeMap<String,usize>)->Result<MemoryRpcRepoState,GitError>{
    read_memory_rpc_repo_state_with(repo,cache,&mut BTreeMap::new())
}
pub fn read_memory_rpc_repo_state_with(repo:&GitMemoryRepo,cache:&mut BTreeMap<String,usize>,tree_stats:&mut BTreeMap<String,MemoryTreeStats>)->Result<MemoryRpcRepoState,GitError>{
    let head=repo.head()?;let dirty_paths=repo.status(&[] as &[&str]).map(|status|status.lines().filter(|line|!line.trim().is_empty()).count()).unwrap_or(0);
    let mut state=MemoryRpcRepoState{head_sha:head.clone(),head_subject:None,committed_at_iso:None,dirty:dirty_paths>0,dirty_paths,system_tokens_estimate:0,total_bytes:None,system_bytes:None,file_count:None,entries_today:None,previous_entry_at_iso:None};
    let Some(head)=head else{return Ok(state);};
    state.head_subject=repo.log(Some(&memory_core::git::GitLogOptions{limit:Some(1),..Default::default()})).ok().and_then(|entries|entries.first().map(|entry|entry.subject.clone()));
    state.committed_at_iso=repo.head_commit_timestamp().ok().flatten().and_then(|timestamp|chrono::DateTime::from_timestamp(timestamp,0)).map(|time|time.to_rfc3339_opts(chrono::SecondsFormat::Millis,true));
    let stats=match tree_stats.get(&head){Some(stats)=>Some(stats.clone()),None=>repo.ls_tree_sized(Some(&head)).ok().map(|entries|memory_tree_stats(&entries))};
    match stats{
        Some(stats)=>{
            state.system_tokens_estimate=usize::try_from(stats.system_markdown_bytes/4).unwrap_or(usize::MAX);
            cache.insert(head.clone(),state.system_tokens_estimate);
            (state.total_bytes,state.system_bytes,state.file_count)=(Some(stats.total_bytes),Some(stats.system_bytes),Some(stats.file_count));
            tree_stats.insert(head,stats);
        }
        None=>state.system_tokens_estimate=cache.get(&head).copied().unwrap_or(0),
    }
    state.entries_today=count_entries_today(repo);
    state.previous_entry_at_iso=read_previous_entry_at(repo);
    Ok(state)
}
/// Commits since LOCAL midnight - "today" is the user's day, not UTC's (pin `countEntriesToday`).
fn count_entries_today(repo:&GitMemoryRepo)->Option<u64>{
    let midnight=chrono::Local::now().date_naive().and_hms_opt(0,0,0)?.and_local_timezone(chrono::Local).earliest()?;
    let since=format!("--since={}",midnight.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Millis,true));
    let result=repo.exec().run_in(&repo.dir,&["rev-list","--count",&since,"HEAD"]).ok()?;
    if result.code!=0{return None;}
    result.stdout.trim().parse::<u64>().ok()
}
/// The second-newest commit's time (pin `readPreviousEntryAt`).
fn read_previous_entry_at(repo:&GitMemoryRepo)->Option<String>{
    let commits=repo.log(Some(&memory_core::git::GitLogOptions{limit:Some(2),..Default::default()})).ok()?;
    commits.get(1).map(|commit|commit.committed_at.clone()).filter(|committed_at|!committed_at.is_empty())
}
/// Newest `merged` reflection completion; the only outcome that changed committed memory (pin
/// `readLastConsolidationAt`).
pub fn read_last_consolidation_at(context:&MemoryIdentityContext)->Option<String>{
    let entries=std::fs::read_dir(context.identity_paths.reflection.join("completions")).ok()?;
    let mut newest:Option<(chrono::DateTime<chrono::FixedOffset>,String)>=None;
    for entry in entries.filter_map(Result::ok).filter(|entry|entry.file_name().to_string_lossy().ends_with(".json")){
        let Some(record)=std::fs::read(entry.path()).ok().and_then(|bytes|serde_json::from_slice::<serde_json::Value>(&bytes).ok()) else{continue;};
        if record.get("outcome").and_then(serde_json::Value::as_str)!=Some("merged"){continue;}
        let Some(finished_at)=record.get("finishedAt").and_then(serde_json::Value::as_str) else{continue;};
        let Ok(at)=chrono::DateTime::parse_from_rfc3339(finished_at) else{continue;};
        if newest.as_ref().is_none_or(|(current,_)|at>*current){newest=Some((at,finished_at.to_owned()));}
    }
    newest.map(|(_,finished_at)|finished_at)
}
pub fn read_memory_rpc_health(context:&MemoryIdentityContext,now:i64)->serde_json::Value{
    let health=crate::worker::health::read_reflection_health(&context.identity_paths.reflection.join("completions"),crate::status::MEMORY_HEALTH_SCAN_LIMIT,now);
    let mut result=serde_json::json!({"streak":health.streak});
    if !health.fingerprint.is_empty(){result["fingerprint"]=health.fingerprint.into();}
    if let Some(last)=health.last_outcome{let mut value=serde_json::json!({"runId":last.run_id,"outcome":last.outcome,"finishedAt":last.finished_at});if let Some(reason)=last.reason{value["reason"]=reason.into();}result["lastOutcome"]=value;}
    result
}
#[cfg(test)]
mod tests{
    use super::*;
    fn context(root:&std::path::Path)->MemoryIdentityContext{MemoryIdentityContext::new("agent".into(),memory_core::identity::layout::build_identity_paths(root,"agent"),crate::binding::MemorySessionBinding{identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:0.0})}
    #[test]fn real_repo_estimates_committed_system_only_and_cache_by_sha(){let root=tempfile::tempdir().unwrap();let context=context(root.path());let engine=crate::engine_session::prepare_memory_engine_session("agent",&context.identity_paths,Default::default()).unwrap();let mut cache=BTreeMap::new();let state=read_memory_rpc_repo_state(&engine.repo,&mut cache).unwrap();assert!(!state.dirty);assert!(state.head_subject.is_some());assert!(state.committed_at_iso.as_ref().unwrap().ends_with('Z'));assert!(state.system_tokens_estimate>0);std::fs::write(context.identity_paths.repo.join("system/persona.md"),"uncommitted change").unwrap();let next=read_memory_rpc_repo_state(&engine.repo,&mut cache).unwrap();assert!(next.dirty);assert_eq!(next.dirty_paths,1);assert_eq!(state.system_tokens_estimate,next.system_tokens_estimate);assert_eq!(cache.len(),1);}
    #[test]fn journal_counts_floor_and_pending_is_or(){let root=tempfile::tempdir().unwrap();let context=context(root.path());context.ledger.lock().unwrap().pending_compaction=true;assert!(read_memory_rpc_journal_state(&context,"session").pending_compaction);let dir=context.identity_paths.transcripts.join("session");std::fs::create_dir_all(&dir).unwrap();std::fs::write(dir.join("state.json"),r#"{"steps_since_last_successful_reflection":3.9,"pending_compaction":false,"total_completed_steps":-1,"reflected_completed_steps":"2"}"#).unwrap();let state=read_memory_rpc_journal_state(&context,"session");assert_eq!(state,MemoryRpcJournalState{backlog_steps:3.0,pending_compaction:true,total_steps:0.0,reflected_steps:0.0});for malformed in ["{","[]","null"]{std::fs::write(dir.join("state.json"),malformed).unwrap();assert_eq!(read_memory_rpc_journal_state(&context,"session"),MemoryRpcJournalState{pending_compaction:true,..Default::default()});}}

    #[test]fn tree_stats_isolate_system_bytes_and_group_by_top_level_path(){
        let entry=|path:&str,bytes:u64|memory_core::git::GitTreeSizedEntry{path:path.into(),bytes};
        let stats=memory_tree_stats(&[entry("system/persona.md",40),entry("system/nested/guide.md",8),entry("notes.md",20),entry("journal/day.md",12)]);
        assert_eq!(stats,MemoryTreeStats{total_bytes:80,file_count:4,system_bytes:48,system_markdown_bytes:48,by_top_level:BTreeMap::from([("system".into(),48),("notes.md".into(),20),("journal".into(),12)])});
        let stats=memory_tree_stats(&[entry("system/persona.md",40),entry("system/theme.css",25)]);
        assert_eq!((stats.system_bytes,stats.system_markdown_bytes),(65,40));
    }
    #[test]fn repo_state_carries_size_and_timeline_from_one_tree_walk(){
        let root=tempfile::tempdir().unwrap();let context=context(root.path());
        let engine=crate::engine_session::prepare_memory_engine_session("agent",&context.identity_paths,Default::default()).unwrap();
        let mut stats=BTreeMap::new();
        let first=read_memory_rpc_repo_state_with(&engine.repo,&mut BTreeMap::new(),&mut stats).unwrap();
        assert!(first.file_count.unwrap()>0&&first.total_bytes.unwrap()>=first.system_bytes.unwrap());
        assert_eq!(first.system_tokens_estimate as u64,stats.values().next().unwrap().system_markdown_bytes/4);
        assert!(first.entries_today.is_some(),"entries_today is computed");
        assert!(first.previous_entry_at_iso.is_none(),"one commit has no previous entry");
        let author=memory_core::git::GitCommitAuthor{agent_id:"agent".into(),author_name:"Agent".into(),author_email:None};
        std::fs::create_dir_all(context.identity_paths.repo.join("knowledge")).unwrap();
        std::fs::write(context.identity_paths.repo.join("knowledge/fact.md"),"---\ndescription: Fact\n---\nbody\n").unwrap();
        engine.repo.commit_write(&["knowledge/fact.md"],"save fact",&author).unwrap();
        let second=read_memory_rpc_repo_state_with(&engine.repo,&mut BTreeMap::new(),&mut stats).unwrap();
        // Relative to `first`: a run that crosses midnight between the two reads still holds.
        assert_eq!(second.entries_today,first.entries_today.map(|n|n+1));
        assert_eq!(second.file_count,Some(first.file_count.unwrap()+1));
        let seed=engine.repo.log(Some(&memory_core::git::GitLogOptions{limit:Some(2),..Default::default()})).unwrap();
        assert_eq!(second.previous_entry_at_iso.as_deref(),Some(seed[1].committed_at.as_str()));
        assert_eq!(stats.len(),2,"stats are cached per HEAD");
        let json=serde_json::to_value(&second).unwrap();
        assert!(json.get("totalBytes").is_some()&&json.get("previousEntryAtISO").is_some()&&json.get("entriesToday").is_some());
    }
    #[test]fn last_consolidation_is_the_newest_merged_completion(){
        let root=tempfile::tempdir().unwrap();let context=context(root.path());
        assert_eq!(read_last_consolidation_at(&context),None,"no completions directory");
        let dir=context.identity_paths.reflection.join("completions");std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.json"),r#"{"outcome":"merged","finishedAt":"2026-08-13T12:00:00.000Z"}"#).unwrap();
        std::fs::write(dir.join("b.json"),r#"{"outcome":"failed","finishedAt":"2026-08-14T12:00:00.000Z"}"#).unwrap();
        std::fs::write(dir.join("c.json"),r#"{"outcome":"merged","finishedAt":"2026-08-12T12:00:00.000Z"}"#).unwrap();
        std::fs::write(dir.join("d.json"),"{corrupt").unwrap();
        assert_eq!(read_last_consolidation_at(&context).as_deref(),Some("2026-08-13T12:00:00.000Z"));
        std::fs::remove_file(dir.join("a.json")).unwrap();std::fs::remove_file(dir.join("c.json")).unwrap();
        assert_eq!(read_last_consolidation_at(&context),None,"only non-merged completions");
    }
}
