use std::fs;
use serde::Serialize;
use crate::{host_daemon_paths::{HostDaemonPaths,generation_paths},host_daemon_state::{parse_json,read_file_or_undefined},host_reservations::{process_is_live,claim_owner_is_live,read_session_path_claims},host_process_metrics::read_host_process_metrics};
#[derive(Debug,Serialize)]
#[serde(rename_all="camelCase")]
pub struct HostGenerationRow{pub instance_id:String,pub generation:f64,pub pid:u32,pub engine_version:Option<String>,#[serde(rename="rss_mb")]pub rss_mb:Option<i64>,pub sessions:usize,pub current:bool,pub alive:bool}
#[derive(Debug,Default,PartialEq)]
pub struct PrunedDaemonState{pub generations:Vec<String>,pub claims:Vec<String>,pub pointer:bool}
fn current_id(paths:&HostDaemonPaths)->Option<String>{let raw=read_file_or_undefined(&paths.pointer_file).ok().flatten();parse_json(raw.as_deref())?.get("instance_id")?.as_str().map(str::to_owned)}
fn ids(paths:&HostDaemonPaths)->Vec<String>{fs::read_dir(&paths.generations_dir).into_iter().flatten().filter_map(Result::ok).map(|entry|entry.file_name().to_string_lossy().into_owned()).collect()}
pub fn prune_dead_generations(paths:&HostDaemonPaths)->std::io::Result<PrunedDaemonState>{
    let pointed=current_id(paths);let mut pruned=PrunedDaemonState::default();
    for id in ids(paths){let generation=generation_paths(paths,&id);let raw=read_file_or_undefined(&generation.pid_file)?;let Some(record)=parse_json(raw.as_deref())else{continue;};let Some(pid)=record.get("pid").and_then(serde_json::Value::as_u64).and_then(|pid|u32::try_from(pid).ok())else{continue;};if process_is_live(pid){continue;}let _=fs::remove_dir_all(&generation.dir);pruned.generations.push(id);}
    pruned.pointer=pointed.is_some_and(|id|pruned.generations.contains(&id));if pruned.pointer{let _=fs::remove_file(&paths.pointer_file);}
    for claim in read_session_path_claims(&paths.reservations_dir){if claim_owner_is_live(&claim.owner){continue;}let _=fs::remove_file(claim.file);pruned.claims.push(claim.owner.session_path);}
    Ok(pruned)
}
pub fn read_generation_rows(paths:&HostDaemonPaths)->std::io::Result<Vec<HostGenerationRow>>{
    let current=current_id(paths);let claims=read_session_path_claims(&paths.reservations_dir);let mut rows=Vec::new();
    for instance_id in ids(paths){let raw=read_file_or_undefined(&generation_paths(paths,&instance_id).pid_file)?;let Some(record)=parse_json(raw.as_deref())else{continue;};let Some(pid)=record.get("pid").and_then(serde_json::Value::as_u64).and_then(|pid|u32::try_from(pid).ok())else{continue;};if !process_is_live(pid){continue;}rows.push(HostGenerationRow{generation:record.get("generation").and_then(serde_json::Value::as_f64).unwrap_or(0.),pid,engine_version:record.get("engineVersion").and_then(serde_json::Value::as_str).map(str::to_owned),rss_mb:read_host_process_metrics(pid,"linux").rss_mb,sessions:claims.iter().filter(|claim|claim.owner.instance_id==instance_id).count(),current:current.as_ref()==Some(&instance_id),instance_id,alive:true});}
    rows.sort_by(|a,b|a.generation.total_cmp(&b.generation));Ok(rows)
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn prune_preserves_live_and_malformed_records(){let temp=tempfile::tempdir().unwrap();let paths=crate::host_daemon_paths::create_host_daemon_paths("socket",temp.path());crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();for id in ["dead","live","partial"]{crate::host_daemon_paths::create_generation_directory(&generation_paths(&paths,id)).unwrap();}fs::write(generation_paths(&paths,"dead").pid_file,r#"{"pid":0}"#).unwrap();fs::write(generation_paths(&paths,"live").pid_file,serde_json::json!({"pid":std::process::id(),"generation":2}).to_string()).unwrap();fs::write(generation_paths(&paths,"partial").pid_file,"{").unwrap();fs::write(&paths.pointer_file,r#"{"instance_id":"dead"}"#).unwrap();let pruned=prune_dead_generations(&paths).unwrap();assert_eq!(pruned.generations,vec!["dead"]);assert!(pruned.pointer);assert!(generation_paths(&paths,"partial").dir.exists());let rows=read_generation_rows(&paths).unwrap();assert_eq!(rows.len(),1);assert_eq!(rows[0].instance_id,"live");assert!(!rows[0].current);}
}
