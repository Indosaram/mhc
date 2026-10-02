use std::path::Path;
use super::{model_miss::{ModelMissResult,is_retryable_model_miss},run_artifacts::{RunLaunchManifest,RunOutcome,write_run_json_atomic,unlink_run_artifact},run_terminal_claim::{RunTerminalClaimKind,claim_run_terminal,read_run_terminal_claim,run_terminal_claim_matches,run_terminal_claim_path}};

/// The caller supplies the source's unbounded terminal gate. The native core's
/// finite-only wait options cannot yet supply that gate without changing its API.
pub fn publish_run_outcome(run_dir: &Path, manifest: &RunLaunchManifest, outcome: &RunOutcome, gate: impl FnOnce(&mut dyn FnMut() -> Result<(),String>) -> Result<(),String>) -> Result<(),String> {
    let attempt=i64::from(manifest.attempt);
    let claim=claim_run_terminal(run_dir,&manifest.run_id,attempt,RunTerminalClaimKind::Publish,std::process::id()).map_err(|error|error.to_string())?;
    if !run_terminal_claim_matches(&claim,&manifest.run_id,attempt,Some(RunTerminalClaimKind::Publish)) { return Ok(()); }
    write_run_json_atomic(&run_dir.join("publishing.json"),&serde_json::json!({"version":1,"runId":manifest.run_id,"attempt":manifest.attempt,"finishedAt":outcome.finished_at}),0o600).map_err(|error|error.to_string())?;
    gate(&mut || {
        if run_dir.join("final.json").exists() || run_dir.join("abandoned.json").exists() { return Ok(()); }
        let claim=read_run_terminal_claim(run_dir).map_err(|error|error.to_string())?;
        if !run_terminal_claim_matches(&claim,&manifest.run_id,attempt,Some(RunTerminalClaimKind::Publish)) { return Ok(()); }
        let stdout=std::fs::read_to_string(&manifest.stdout_path).map_err(|error|error.to_string())?;
        let stderr=std::fs::read_to_string(&manifest.stderr_path).map_err(|error|error.to_string())?;
        let retrying=is_retryable_model_miss(&ModelMissResult {code:outcome.child_exit.code,stdout:&stdout,stderr:&stderr,timed_out:outcome.timed_out});
        if retrying && let Some(next)=&manifest.next_attempt {
            let ledger=run_dir.join("ledger.json");
            let mut fields=serde_json::Map::from_iter([("attempt".into(),next.attempt.into()),("model".into(),next.model.clone().into()),("launching".into(),true.into())]);
            if let Some(thinking)=&next.thinking { fields.insert("thinking".into(),thinking.clone().into()); }
            // JS undefined removes these keys from the JSON serialization.
            let mut current: serde_json::Map<String,serde_json::Value>=super::run_artifacts::read_run_json(&ledger).map_err(|error|error.to_string())?;
            for key in ["pid","processStart","childPid","childProcessStart"] { current.remove(key); }
            if next.thinking.is_none() { current.remove("thinking"); }
            current.extend(fields);
            write_run_json_atomic(&ledger,&current,0o600).map_err(|error|error.to_string())?;
        }
        write_run_json_atomic(&run_dir.join("outcome.json"),outcome,0o600).map_err(|error|error.to_string())?;
        if retrying { unlink_run_artifact(&run_terminal_claim_path(run_dir)).map_err(|error|error.to_string())?; }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::run_artifacts::{RunKind,RunAttempt,ChildExit,read_run_json};
    fn fixture(root:&Path)->(RunLaunchManifest,RunOutcome) {
        let stdout=root.join("stdout"); let stderr=root.join("stderr"); std::fs::write(&stdout,"").unwrap(); std::fs::write(&stderr,"").unwrap();
        write_run_json_atomic(&root.join("ledger.json"),&serde_json::json!({"attempt":1,"model":"old","thinking":"high","pid":42,"processStart":"start","childPid":43,"childProcessStart":"child"}),0o600).unwrap();
        (RunLaunchManifest {version:1,run_id:"run".into(),attempt:1,next_attempt:None,kind:RunKind::Reflection,command:"child".into(),args:vec![],cwd:root.to_string_lossy().into_owned(),env:Default::default(),hard_deadline_at:100.0,termination_grace_ms:10.0,max_output_bytes:100,stdout_path:stdout.to_string_lossy().into_owned(),stderr_path:stderr.to_string_lossy().into_owned()},RunOutcome {version:1,run_id:"run".into(),attempt:Some(1),finished_at:"now".into(),child_exit:ChildExit {code:Some(0),signal:None},timed_out:false})
    }
    #[test] fn publication_marks_intent_before_gate_and_keeps_success_claim() {
        let root=tempfile::tempdir().unwrap(); let (manifest,outcome)=fixture(root.path());
        publish_run_outcome(root.path(),&manifest,&outcome,|operation| {assert!(root.path().join("publishing.json").exists()); assert!(!root.path().join("outcome.json").exists()); operation()}).unwrap();
        let published:RunOutcome=read_run_json(&root.path().join("outcome.json")).unwrap(); assert_eq!(published.attempt,Some(1)); assert!(run_terminal_claim_path(root.path()).exists());
    }
    #[test] fn retry_advances_ledger_and_removes_old_process_and_thinking_fields() {
        let root=tempfile::tempdir().unwrap(); let (mut manifest,mut outcome)=fixture(root.path());
        manifest.next_attempt=Some(RunAttempt {attempt:2,model:"next".into(),thinking:None}); outcome.child_exit.code=Some(1);
        std::fs::write(&manifest.stderr_path,"No API key found for provider").unwrap();
        publish_run_outcome(root.path(),&manifest,&outcome,|operation|operation()).unwrap();
        let ledger:serde_json::Value=read_run_json(&root.path().join("ledger.json")).unwrap(); assert_eq!(ledger["attempt"],2); assert_eq!(ledger["model"],"next"); assert_eq!(ledger["launching"],true);
        for key in ["thinking","pid","processStart","childPid","childProcessStart"] {assert!(ledger.get(key).is_none());}
        assert!(!run_terminal_claim_path(root.path()).exists());
    }
    #[test] fn terminal_winner_prevents_outcome_publication() {
        let root=tempfile::tempdir().unwrap(); let (manifest,outcome)=fixture(root.path());
        publish_run_outcome(root.path(),&manifest,&outcome,|operation| {write_run_json_atomic(&root.path().join("abandoned.json"),&serde_json::json!({}),0o600).unwrap(); operation()}).unwrap();
        assert!(!root.path().join("outcome.json").exists());
    }
    #[test] fn competing_abandon_claim_never_enters_gate() {
        let root=tempfile::tempdir().unwrap(); let (manifest,outcome)=fixture(root.path());
        claim_run_terminal(root.path(),"run",1,RunTerminalClaimKind::Abandon,std::process::id()).unwrap();
        publish_run_outcome(root.path(),&manifest,&outcome,|_|panic!("gate should not run")).unwrap(); assert!(!root.path().join("publishing.json").exists());
    }
}
