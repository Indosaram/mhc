use std::path::Path;
use serde_json::json;
use chrono::{DateTime,Utc,SecondsFormat};
use crate::host_crash_record::record_host_crash;
#[derive(Debug,PartialEq,Eq)]
pub struct ChildExitVerdict { pub reason:String,pub exit_code:i32 }
pub fn classify_child_exit(code:Option<i32>,signal:Option<&str>) -> ChildExitVerdict {
    if code == Some(0) && signal.is_none() { return ChildExitVerdict { reason:"rpc host exited on its own idle policy".into(),exit_code:0 }; }
    let detail=code.map(|code| code.to_string()).unwrap_or_else(|| signal.unwrap_or("null").into());
    ChildExitVerdict { reason:format!("rpc host process exited unexpectedly ({detail})"),exit_code:1 }
}
pub fn note_child_exit(daemon_dir:&Path,code:Option<i32>,signal:Option<&str>,child_started_at:i64,now:i64) {
    if classify_child_exit(code,signal).exit_code == 0 { return; }
    let Some(at)=DateTime::<Utc>::from_timestamp_millis(now) else { return; };
    let mut record=json!({"at":at.to_rfc3339_opts(SecondsFormat::Millis,true),"uptimeMs":now.saturating_sub(child_started_at).max(0)});
    if let Some(signal)=signal { record["signal"]=json!(signal); } else if let Some(code)=code { record["code"]=json!(code); }
    record_host_crash(daemon_dir,&record);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn only_zero_without_signal_is_clean() { assert_eq!(classify_child_exit(Some(0),None).exit_code,0);for (code,signal) in [(Some(1),None),(None,Some("SIGBUS")),(Some(0),Some("SIGTERM")),(None,None)] { assert_eq!(classify_child_exit(code,signal).exit_code,1); } }
}
