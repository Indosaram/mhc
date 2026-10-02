use std::{fs, io::{self, Write}, path::{Path,PathBuf}};
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use crate::git_helpers::git_common_dir_realpath;
use crate::constants::{COOLDOWN_DAYS,MS_PER_DAY};
const GLOBAL: &str="init-deep-advisor-declined-global";
const PROJECTS: &str="init-deep-advisor-declined-projects";
const COOLDOWNS: &str="init-deep-advisor-cooldowns";
const PROPOSALS: &str="init-deep-advisor-proposals";
#[derive(Debug,Clone,Serialize,Deserialize,PartialEq)]
#[serde(rename_all="camelCase")]
pub struct InitDeepSnapshotV1 { pub commit_sha:String,pub file_count:f64,pub loc:f64,pub timestamp:f64,pub mode:SuggestedMode }
#[derive(Debug,Clone,Copy,Serialize,Deserialize,PartialEq,Eq)]
#[serde(rename_all="lowercase")]
pub enum SuggestedMode { Local,Committed }
#[derive(Debug,PartialEq)]
pub enum SnapshotReadResult { Missing,Invalid,Valid(InitDeepSnapshotV1) }
pub fn get_advisor_state_dir(native_state_dir: &Path) -> PathBuf { native_state_dir.join("init-deep-advisor-state") }
pub fn repo_hash(root: &Path) -> io::Result<String> { Ok(format!("{:x}",Sha256::digest(git_common_dir_realpath(root)?.to_string_lossy().as_bytes()))) }
fn write_atomic(path: &Path, value: Value) -> io::Result<()> {
    let parent=path.parent().ok_or_else(||io::Error::other("state path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut file=tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(serde_json::to_string(&value)?.as_bytes())?;
    file.persist(path).map_err(|e|e.error)?;
    Ok(())
}
fn read_json(path: &Path) -> Option<Value> { serde_json::from_str(&fs::read_to_string(path).ok()?).ok() }
pub fn write_global_decline(dir:&Path,now:f64) -> io::Result<()> { write_atomic(&dir.join(GLOBAL),json!({"declinedAt":now})) }
pub fn is_globally_declined(dir:&Path) -> bool { dir.join(GLOBAL).exists() }
pub fn write_project_decline(dir:&Path,repo:&str,now:f64) -> io::Result<()> { write_atomic(&dir.join(PROJECTS).join(repo),json!({"declinedAt":now})) }
pub fn is_project_declined(dir:&Path,repo:&str) -> bool { dir.join(PROJECTS).join(repo).exists() }
pub fn write_cooldown(dir:&Path,repo:&str,at:f64) -> io::Result<()> { let duration=(COOLDOWN_DAYS*MS_PER_DAY).to_string().parse::<f64>().map_err(io::Error::other)?;write_atomic(&dir.join(COOLDOWNS).join(repo),json!({"until":at+duration})) }
pub fn read_cooldown_until(dir:&Path,repo:&str) -> f64 { read_json(&dir.join(COOLDOWNS).join(repo)).and_then(|v|v.get("until").and_then(Value::as_f64)).filter(|n|n.is_finite() && *n>=0.0).unwrap_or(0.0) }
pub fn is_cooling_down(dir:&Path,repo:&str,now:f64) -> bool { now<read_cooldown_until(dir,repo) }
pub fn write_last_proposed_head(dir:&Path,repo:&str,head:&str,now:f64) -> io::Result<()> { write_atomic(&dir.join(PROPOSALS).join(repo),json!({"lastProposedHead":head,"lastProposedAt":now})) }
pub fn read_last_proposed_head(dir:&Path,repo:&str) -> Option<String> { read_json(&dir.join(PROPOSALS).join(repo)).and_then(|v|v.get("lastProposedHead").and_then(Value::as_str).map(str::to_owned)) }
pub fn read_snapshot(root:&Path) -> SnapshotReadResult {
    let raw=match fs::read_to_string(root.join(".omo/init-deep.json")) { Ok(s)=>s,Err(e) if e.kind()==io::ErrorKind::NotFound=>return SnapshotReadResult::Missing,Err(_)=>return SnapshotReadResult::Invalid };
    let Ok(snapshot)=serde_json::from_str::<InitDeepSnapshotV1>(&raw) else { return SnapshotReadResult::Invalid; };
    if [snapshot.file_count,snapshot.loc,snapshot.timestamp].iter().any(|n|!n.is_finite() || *n<0.0) { return SnapshotReadResult::Invalid; }
    SnapshotReadResult::Valid(snapshot)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_helpers::{run,tests::repo};
    fn snapshot(root:&Path,raw:&str) { fs::create_dir_all(root.join(".omo")).unwrap(); fs::write(root.join(".omo/init-deep.json"),raw).unwrap(); }
    fn valid() -> Value { json!({"commitSha":"bbbb","fileCount":12,"loc":3400,"timestamp":1700000000000_u64,"mode":"committed"}) }
    #[test] fn state_dir() { assert!(get_advisor_state_dir(Path::new("home/omo-senpi/omo-native")).ends_with("omo-native/init-deep-advisor-state")); }
    #[test] fn hash_stable() { let (t,_)=repo(); let a=repo_hash(t.path()).unwrap(); assert_eq!(a,repo_hash(t.path()).unwrap()); assert_eq!(a.len(),64); assert!(a.bytes().all(|b|b.is_ascii_hexdigit())); }
    #[test] fn hash_distinct() { let (a,_)=repo(); let (b,_)=repo(); assert_ne!(repo_hash(a.path()).unwrap(),repo_hash(b.path()).unwrap()); }
    #[test] fn worktree_hash() { let (t,_)=repo(); let p=tempfile::tempdir().unwrap(); let w=p.path().join("linked"); run(t.path(),&["worktree","add","-b","linked",w.to_str().unwrap()]).unwrap(); assert_eq!(repo_hash(t.path()).unwrap(),repo_hash(&w).unwrap()); }
    #[test] fn global_decline() { let t=tempfile::tempdir().unwrap(); assert!(!is_globally_declined(t.path())); write_global_decline(t.path(),0.0).unwrap(); assert!(is_globally_declined(t.path())); }
    #[test] fn project_decline_creates_parent() { let t=tempfile::tempdir().unwrap(); let dir=t.path().join("nested/advisor"); write_project_decline(&dir,"hash-a",0.0).unwrap(); assert!(is_project_declined(&dir,"hash-a")); }
    #[test] fn project_isolation() { let t=tempfile::tempdir().unwrap(); write_project_decline(t.path(),"hash-a",0.0).unwrap(); assert!(!is_project_declined(t.path(),"hash-b")); }
    #[test] fn cooldown_deadline() { let t=tempfile::tempdir().unwrap(); write_cooldown(t.path(),"a",1000.0).unwrap(); assert!((read_cooldown_until(t.path(),"a")-604801000.0).abs()<f64::EPSILON); assert!(is_cooling_down(t.path(),"a",1001.0)); assert!(!is_cooling_down(t.path(),"a",604801000.0)); assert_eq!(COOLDOWN_DAYS*MS_PER_DAY,604800000); }
    #[test] fn cooldown_missing() { let t=tempfile::tempdir().unwrap(); assert!(!is_cooling_down(t.path(),"missing",1.0)); }
    #[test] fn cooldown_malformed() { let t=tempfile::tempdir().unwrap(); fs::create_dir(t.path().join(COOLDOWNS)).unwrap(); fs::write(t.path().join(COOLDOWNS).join("bad"),"not json").unwrap(); assert!(read_cooldown_until(t.path(),"bad").abs()<f64::EPSILON); }
    #[test] fn cooldown_wrong_type() { let t=tempfile::tempdir().unwrap(); write_atomic(&t.path().join(COOLDOWNS).join("typed"),json!({"until":"soon"})).unwrap(); assert!(read_cooldown_until(t.path(),"typed").abs()<f64::EPSILON); }
    #[test] fn head_roundtrip() { let t=tempfile::tempdir().unwrap(); write_last_proposed_head(t.path(),"a","head",0.0).unwrap(); assert_eq!(read_last_proposed_head(t.path(),"a").as_deref(),Some("head")); }
    #[test] fn head_missing() { let t=tempfile::tempdir().unwrap(); assert!(read_last_proposed_head(t.path(),"missing").is_none()); }
    #[test] fn head_malformed() { let t=tempfile::tempdir().unwrap(); fs::create_dir(t.path().join(PROPOSALS)).unwrap(); fs::write(t.path().join(PROPOSALS).join("bad"),"{not json").unwrap(); write_atomic(&t.path().join(PROPOSALS).join("typed"),json!({"lastProposedHead":42})).unwrap(); assert!(read_last_proposed_head(t.path(),"bad").is_none()); assert!(read_last_proposed_head(t.path(),"typed").is_none()); }
    #[test] fn snapshot_missing() { let t=tempfile::tempdir().unwrap(); assert_eq!(read_snapshot(t.path()),SnapshotReadResult::Missing); }
    #[test] fn snapshot_valid() { let t=tempfile::tempdir().unwrap(); snapshot(t.path(),&valid().to_string()); assert!(matches!(read_snapshot(t.path()),SnapshotReadResult::Valid(_))); }
    #[test] fn snapshot_extra_fields() { let t=tempfile::tempdir().unwrap(); let mut v=valid(); v["futureField"]=json!({"nested":true}); snapshot(t.path(),&v.to_string()); assert!(matches!(read_snapshot(t.path()),SnapshotReadResult::Valid(_))); }
    #[test] fn snapshot_bad_json() { let t=tempfile::tempdir().unwrap(); snapshot(t.path(),"{not json"); assert_eq!(read_snapshot(t.path()),SnapshotReadResult::Invalid); }
    #[test] fn snapshot_nonobject() { let t=tempfile::tempdir().unwrap(); for raw in ["[]","null","true","\"a string\"","42"] { snapshot(t.path(),raw); assert_eq!(read_snapshot(t.path()),SnapshotReadResult::Invalid); } }
    #[test] fn snapshot_missing_field() { let t=tempfile::tempdir().unwrap(); let mut v=valid(); v.as_object_mut().unwrap().remove("timestamp"); snapshot(t.path(),&v.to_string()); assert_eq!(read_snapshot(t.path()),SnapshotReadResult::Invalid); }
    #[test] fn snapshot_wrong_types() { let t=tempfile::tempdir().unwrap(); let mut v=valid(); v["commitSha"]=json!(12345); v["fileCount"]=json!("1"); snapshot(t.path(),&v.to_string()); assert_eq!(read_snapshot(t.path()),SnapshotReadResult::Invalid); }
    #[test] fn snapshot_negative_nonfinite() { let t=tempfile::tempdir().unwrap(); let mut v=valid(); v["fileCount"]=json!(-1); snapshot(t.path(),&v.to_string()); assert_eq!(read_snapshot(t.path()),SnapshotReadResult::Invalid); snapshot(t.path(),"{\"commitSha\":\"eeee\",\"fileCount\":1,\"loc\":1e999,\"timestamp\":3,\"mode\":\"local\"}"); assert_eq!(read_snapshot(t.path()),SnapshotReadResult::Invalid); }
    #[test] fn snapshot_invalid_mode() { let t=tempfile::tempdir().unwrap(); let mut v=valid(); v["mode"]=json!("hybrid"); snapshot(t.path(),&v.to_string()); assert_eq!(read_snapshot(t.path()),SnapshotReadResult::Invalid); }
}
