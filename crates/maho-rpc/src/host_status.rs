use serde::Serialize;
use serde_json::Value;
use crate::host_protocol_info::HostProtocolInfo;
#[derive(Debug,Default,Serialize,PartialEq,Eq)]
pub struct HostSessionCounts{pub total:usize,pub interactive:usize,pub worker:usize,pub retained:usize,pub foreign_attached:usize,pub foreign_retained:usize}
pub fn session_counts(reply:&Value)->HostSessionCounts{
    let mut counts=HostSessionCounts::default();
    if let Some(rows)=reply.get("sessions").and_then(Value::as_array){for row in rows.iter().filter(|row|row.is_object()){
        counts.total+=1;
        if row.get("kind").and_then(Value::as_str)==Some("worker"){counts.worker+=1;}else{counts.interactive+=1;}
        if row.get("attachments").and_then(Value::as_f64).unwrap_or(0.)>0.{counts.foreign_attached+=1;}else{counts.retained+=1;counts.foreign_retained+=1;}
    }}counts
}
pub async fn read_session_counts(socket:&str,include_workers:bool)->HostSessionCounts{
    let mut command=serde_json::json!({"type":"list_sessions"});if include_workers{command["include_workers"]=true.into();}
    session_counts(&crate::host_probe::request_on_socket(socket,&command,10000).await.unwrap_or(Value::Null))
}
pub fn host_summary(host:Option<&HostProtocolInfo>)->Value{match host{None=>Value::Null,Some(host)=>serde_json::json!({"protocolVersion":host.protocol_version,"serverVersion":host.server_version,"capabilities":host.capabilities,"instanceId":host.instance_id,"generation":host.generation,"engineVersion":host.engine_version,"launchProfileId":host.launch_profile.as_ref().map(|profile|&profile.profile_id)})}}
pub async fn read_host_status(socket:&str,agent_dir:&std::path::Path,include_workers:bool)->std::io::Result<Value>{
    let paths=crate::host_daemon_paths::create_host_daemon_paths(socket,agent_dir);
    let host=crate::host_probe::probe_protocol_info(socket,10000).await;
    crate::host_generations::prune_dead_generations(&paths)?;
    let generations=crate::host_generations::read_generation_rows(&paths)?;
    let current=generations.iter().find(|row|row.current);
    let metrics=current.map(|row|crate::host_process_metrics::read_host_process_metrics(row.pid,"linux")).unwrap_or_default();
    let sessions=read_session_counts(socket,include_workers).await;
    let env_keys=crate::host_daemon_env::read_daemon_env_keys(&paths)?;
    Ok(serde_json::json!({"reachable":host.is_some(),"socket":socket,"pid":current.map(|row|row.pid),"instanceId":host.as_ref().and_then(|host|host.instance_id.as_deref()).or_else(||current.map(|row|row.instance_id.as_str())),"generation":host.as_ref().and_then(|host|host.generation).or_else(||current.map(|row|row.generation)),"engineVersion":host.as_ref().and_then(|host|host.engine_version.as_deref()).or_else(||current.and_then(|row|row.engine_version.as_deref())),"capabilities":host.as_ref().map(|host|host.capabilities.as_slice()).unwrap_or(&[]),"launchProfile":host.as_ref().and_then(|host|host.launch_profile.as_ref()),"sessions":sessions,"zombies":metrics.zombies,"rss_mb":metrics.rss_mb,"open_fds":metrics.open_fds,"env_keys":env_keys,"generations":generations}))
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn defaults_malformed_rows_and_counts_foreign_occupancy(){let counts=session_counts(&serde_json::json!({"sessions":[null,{}, {"kind":"worker","attachments":2},{"kind":"worker","attachments":-1},[]]}));assert_eq!(counts,HostSessionCounts{total:3,interactive:1,worker:2,retained:2,foreign_attached:1,foreign_retained:2});assert_eq!(session_counts(&Value::Null),HostSessionCounts::default());assert_eq!(host_summary(None),Value::Null);}
    #[tokio::test]async fn absent_endpoint_has_complete_nullable_status(){let temp=tempfile::tempdir().unwrap();let socket=temp.path().join("absent.sock");let report=read_host_status(socket.to_str().unwrap(),temp.path(),true).await.unwrap();assert_eq!(report["reachable"],false);assert!(report["pid"].is_null());assert_eq!(report["sessions"]["total"],0);assert_eq!(report["env_keys"],serde_json::json!([]));}
}
