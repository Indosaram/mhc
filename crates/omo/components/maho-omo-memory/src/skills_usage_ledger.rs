use std::{collections::BTreeMap, path::{Path, PathBuf}};
use serde::{Deserialize, Serialize};
use memory_core::locks::{AcquireLockOptions, CreateLockRecordOptions, LockRecord, LockRecordError, WithLockError, create_lock_record, skills_usage_lock_path, with_lock};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillUsageEntry { pub count: f64, pub last_used_at: String }
pub type SkillsUsageLedger = BTreeMap<String, SkillUsageEntry>;
pub struct SkillsUsageLedgerPath { pub ledger_path: PathBuf, pub lock_path: PathBuf }
pub fn skills_usage_paths(runtime:&Path,locks:&Path)->SkillsUsageLedgerPath {SkillsUsageLedgerPath{ledger_path:runtime.join("skills-usage.json"),lock_path:skills_usage_lock_path(locks)}}
pub fn read_skills_usage_ledger(path:&Path)->SkillsUsageLedger {
    let Some(value)=std::fs::read(path).ok().and_then(|bytes|serde_json::from_slice::<serde_json::Value>(&bytes).ok()) else{return BTreeMap::new();};
    let Some(object)=value.as_object() else{return BTreeMap::new();};
    object.iter().filter_map(|(key,value)|Some((key.clone(),SkillUsageEntry{count:value.get("count")?.as_f64()?,last_used_at:value.get("lastUsedAt")?.as_str()?.to_owned()}))).collect()
}
pub fn create_skills_usage_lock_record()->Result<LockRecord,LockRecordError>{create_lock_record("skills-usage",CreateLockRecordOptions::default())}
pub fn increment_skills_usage_batch(paths:&SkillsUsageLedgerPath,increments:&BTreeMap<String,f64>,now:impl FnOnce()->String,record:&LockRecord,aborted:Option<&dyn Fn()->bool>)->Result<(),WithLockError<std::io::Error>> {
    with_lock(&paths.lock_path,record,&AcquireLockOptions{wait_timeout_ms:Some(2000),..Default::default()},|| {
        let mut current=read_skills_usage_ledger(&paths.ledger_path);let timestamp=now();
        for (id,increment) in increments {let count=current.get(id).map_or(0.0,|entry|entry.count)+increment;current.insert(id.clone(),SkillUsageEntry{count,last_used_at:timestamp.clone()});}
        if aborted.is_some_and(|aborted|aborted()){return Ok(());}
        std::fs::create_dir_all(paths.ledger_path.parent().unwrap_or(Path::new(".")))?;
        if aborted.is_some_and(|aborted|aborted()){return Ok(());}
        let temporary=PathBuf::from(format!("{}.tmp",paths.ledger_path.display()));
        std::fs::write(&temporary,serde_json::to_vec_pretty(&current)?)?;
        std::fs::rename(temporary,&paths.ledger_path)
    })
}
pub fn increment_skill_usage(paths:&SkillsUsageLedgerPath,skill_id:&str,now:impl FnOnce()->String,warn:impl FnOnce(&str)) {
    match create_skills_usage_lock_record() {Ok(record)=>{if let Err(error)=increment_skills_usage_batch(paths,&BTreeMap::from([(skill_id.to_owned(),1.0)]),now,&record,None){warn(&error.to_string());}},Err(error)=>warn(&error.to_string())}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]fn missing_ledger_is_empty(){let dir=tempfile::tempdir().unwrap();assert!(read_skills_usage_ledger(&dir.path().join("absent")).is_empty());}
    #[test]fn valid_entries_are_parsed(){let dir=tempfile::tempdir().unwrap();let path=dir.path().join("ledger.json");std::fs::write(&path,r#"{"foo":{"count":3,"lastUsedAt":"2026-01-15T10:00:00Z"},"bad":{"count":"3","lastUsedAt":"x"},"extra":{"count":-0.5,"lastUsedAt":"x","other":true}}"#).unwrap();let ledger=read_skills_usage_ledger(&path);assert_eq!(ledger["foo"].count,3.0);assert_eq!(ledger["extra"].count,-0.5);assert_eq!(ledger.len(),2);}
    #[test]fn corrupted_ledger_is_empty(){let dir=tempfile::tempdir().unwrap();let path=dir.path().join("ledger.json");for content in ["not json {{{","[]","null"]{std::fs::write(&path,content).unwrap();assert!(read_skills_usage_ledger(&path).is_empty());}}
    #[test]fn batch_merges_and_releases_lock(){let dir=tempfile::tempdir().unwrap();let paths=skills_usage_paths(&dir.path().join("runtime"),&dir.path().join("locks"));for _ in 0..2 {increment_skill_usage(&paths,"foo",||"2026-01-15T10:00:00Z".into(),|error|panic!("{error}"));}assert_eq!(read_skills_usage_ledger(&paths.ledger_path)["foo"].count,2.0);assert!(!paths.lock_path.exists());assert_eq!(std::fs::read_dir(paths.ledger_path.parent().unwrap()).unwrap().count(),1);}
    #[test]fn aborted_batch_does_not_publish(){let dir=tempfile::tempdir().unwrap();let paths=skills_usage_paths(&dir.path().join("runtime"),&dir.path().join("locks"));let record=create_skills_usage_lock_record().unwrap();increment_skills_usage_batch(&paths,&BTreeMap::from([("foo".into(),1.0)]),||"now".into(),&record,Some(&||true)).unwrap();assert!(!paths.ledger_path.exists());assert!(!paths.lock_path.exists());}
}
