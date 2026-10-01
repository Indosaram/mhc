use serde_json::Value;
#[derive(Clone,Debug)]
pub struct ReservationRunLedger(Value);
impl ReservationRunLedger{pub fn value(&self)->&Value{&self.0}pub fn run_id(&self)->&str{self.0["runId"].as_str().expect("validated runId")}}
#[derive(Debug,PartialEq,Eq)]
pub struct ReservationLedgerError(pub &'static str);
impl std::fmt::Display for ReservationLedgerError{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str(self.0)}}
impl std::error::Error for ReservationLedgerError{}
fn invalid(message:&'static str)->ReservationLedgerError{ReservationLedgerError(message)}
fn one_of(value:Option<&Value>,allowed:&[&str])->bool{value.and_then(Value::as_str).is_some_and(|value|allowed.contains(&value))}
fn positive_integer(value:&Value)->bool{value.as_f64().is_some_and(|value|value.is_finite()&&value.fract()==0.0&&value>0.0)}
pub fn parse_reservation_run_ledger(value:Value)->Result<ReservationRunLedger,ReservationLedgerError>{
    let Some(record)=value.as_object()else{return Err(invalid("Invalid reservation run ledger"));};
    if record.get("version").and_then(Value::as_f64)!=Some(1.0)||!record.get("runId").is_some_and(Value::is_string){return Err(invalid("Invalid reservation run ledger identity"));}
    if !one_of(record.get("kind"),&["reflection","dream"]){return Err(invalid("Invalid reservation run kind"));}
    if !one_of(record.get("trigger"),&["step-count","compaction","manual","dream"]){return Err(invalid("Invalid reservation run trigger"));}
    let dream=record["kind"]=="dream";
    if dream!=(record["trigger"]=="dream"){return Err(invalid("Reservation run kind and trigger disagree"));}
    if dream&&!one_of(record.get("origin"),&["manual","idle","shutdown"]){return Err(invalid("Dream run origin is required"));}
    if ["startedAt","worktreeDir","worktreeBranch","baseSha","gitFilePath","gitFileSnapshot","commonConfigPath"].iter().any(|key|!record.get(*key).is_some_and(Value::is_string)){return Err(invalid("Invalid reservation run paths"));}
    if ["hardDeadlineAt","terminationGraceMs","deadlineAt"].iter().any(|key|!record.get(*key).and_then(Value::as_f64).is_some_and(f64::is_finite)){return Err(invalid("Invalid reservation run deadline"));}
    if record["deadlineAt"].as_f64()!=Some(record["hardDeadlineAt"].as_f64().expect("validated deadline")+record["terminationGraceMs"].as_f64().expect("validated grace")){return Err(invalid("Reservation run deadline mismatch"));}
    if !one_of(record.get("mergePolicy"),&["auto","integration"]){return Err(invalid("Invalid reservation merge policy"));}
    if record.get("targetDoc").is_some_and(|target|!dream||!target.is_string()){return Err(invalid("Invalid dream target document"));}
    if ["pid","childPid"].iter().any(|key|record.get(*key).is_some_and(|value|!positive_integer(value)))||["processStart","childProcessStart"].iter().any(|key|record.get(*key).is_some_and(|value|!value.is_null()&&!value.is_string())){return Err(invalid("Invalid reservation process identity"));}
    if record.get("attempt").is_some_and(|value|!positive_integer(value)){return Err(invalid("Invalid reservation run attempt"));}
    for (key,error) in [("model","Invalid reservation run model"),("thinking","Invalid reservation run thinking"),("category","Invalid reservation run category")]{if record.get(key).is_some_and(|value|!value.is_string()){return Err(invalid(error));}}
    if record.get("conversationIds").is_some_and(|value|!value.as_array().is_some_and(|values|values.iter().all(Value::is_string))){return Err(invalid("Invalid reservation run conversations"));}
    if record.get("launching").is_some_and(|value|!value.is_boolean()){return Err(invalid("Invalid reservation run launching state"));}
    if !record.get("commonConfigSnapshot").is_some_and(|value|value.is_null()||value.is_string()){return Err(invalid("Invalid common config snapshot"));}
    if record.contains_key("finalizePhase")&&!one_of(record.get("finalizePhase"),&["validated","integrated","settled"]){return Err(invalid("Invalid finalization phase"));}
    for (key,error) in [("validatedTipSha","Invalid validatedTipSha"),("integrationSha","Invalid integrationSha"),("finalizeReason","Invalid finalizeReason"),("finalizeDetail","Invalid finalizeDetail"),("finalizedAt","Invalid finalizedAt")]{if record.get(key).is_some_and(|value|!value.is_string()){return Err(invalid(error));}}
    if record.get("validatedChangedPaths").is_some_and(|value|!value.as_array().is_some_and(|values|values.iter().all(Value::is_string))){return Err(invalid("Invalid validated changed paths"));}
    if record.contains_key("finalizeOutcome")&&!one_of(record.get("finalizeOutcome"),&["merged","no_changes","parent_dirty","dirty_uncommitted","merge_conflict","admin_tamper","timed_out","failed"]){return Err(invalid("Invalid finalization outcome"));}
    Ok(ReservationRunLedger(value))
}
pub fn worktree_from_ledger(repo_path:&std::path::Path,identity:&str,ledger:&ReservationRunLedger)->Result<memory_core::reflection::worktree::ReflectionWorktree,memory_core::git::errors::GitError>{
    let value=ledger.value();
    let string=|key:&str|value[key].as_str().expect("validated ledger string").to_owned();
    Ok(memory_core::reflection::worktree::ReflectionWorktree{
        parent:memory_core::git::GitMemoryRepo::open(repo_path,identity)?,
        dir:string("worktreeDir").into(),branch:string("worktreeBranch"),base_commit_sha:string("baseSha"),
        git_file_path:string("gitFilePath").into(),git_file_snapshot:string("gitFileSnapshot"),
        common_config_path:string("commonConfigPath").into(),
        common_config_snapshot:value["commonConfigSnapshot"].as_str().map(str::to_owned),
        exec:memory_core::git::exec::create_git_exec(Default::default()),
    })
}
#[cfg(test)]
mod tests{
    use super::*;
    fn ledger()->Value{serde_json::json!({"version":1,"runId":"run","kind":"reflection","trigger":"manual","startedAt":"now","hardDeadlineAt":100,"terminationGraceMs":5,"deadlineAt":105,"mergePolicy":"auto","worktreeDir":"/tmp/w","worktreeBranch":"memory/run","baseSha":"sha","gitFilePath":"/tmp/w/.git","gitFileSnapshot":"gitdir: /tmp/g","commonConfigPath":"/tmp/g/config","commonConfigSnapshot":null})}
    #[test]fn preserves_unknown_fields_null_and_large_js_integer_pid(){let mut value=ledger();value["future"]=serde_json::json!({"x":1});value["pid"]=serde_json::json!(4_294_967_296u64);value["processStart"]=Value::Null;value["finalizeOutcome"]="admin_tamper".into();let parsed=parse_reservation_run_ledger(value.clone()).unwrap();assert_eq!(parsed.value(),&value);assert_eq!(parsed.run_id(),"run");}
    #[test]fn dream_requires_matching_trigger_origin_and_target(){let mut value=ledger();value["kind"]="dream".into();assert_eq!(parse_reservation_run_ledger(value.clone()).unwrap_err().0,"Reservation run kind and trigger disagree");value["trigger"]="dream".into();assert_eq!(parse_reservation_run_ledger(value.clone()).unwrap_err().0,"Dream run origin is required");value["origin"]="idle".into();value["targetDoc"]="reference/style.md".into();assert!(parse_reservation_run_ledger(value).is_ok());}
    #[test]fn invalid_fields_rejected(){for (key,value) in [("version",Value::from(2)),("deadlineAt",Value::from(106)),("pid",Value::Null),("attempt",Value::from(1.5)),("conversationIds",serde_json::json!([1])),("launching",Value::from("true")),("commonConfigSnapshot",Value::Bool(false)),("finalizeOutcome",Value::from("unknown"))]{let mut input=ledger();input[key]=value;assert!(parse_reservation_run_ledger(input).is_err(),"{key}");}}
}
