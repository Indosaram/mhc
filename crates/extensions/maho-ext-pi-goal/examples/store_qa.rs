use maho_ext_pi_goal::goal::{store::*,format::goal_tool_response,types::*};
fn main()->Result<(),Box<dyn std::error::Error>>{
    let temp=tempfile::tempdir()?;let reference=GoalStoreRef{base_dir:temp.path().join("pi-goal"),thread_id:"qa-thread".into()};
    let created=create_goal_at(&reference,"Fixture objective",100,"first".into())?;
    account_goal_usage_at(&reference,&TokenUsageSnapshot{input:23.0,output:7.0,..Default::default()},3.0,GoalAccountingMode::Active,None,101)?;
    let completed=update_goal_at(&reference,&GoalUpdate{status:Some(GoalStatus::Complete),..Default::default()},GoalUpdateSource::Model,102,"unused".into())?;
    let fetched=read_goal(&reference)?.ok_or("goal missing")?;
    if created.status!=GoalStatus::Active||completed!=fetched||fetched.tokens_used!=30.0||fetched.time_used_seconds!=3.0{return Err("goal QA contract mismatch".into());}
    create_goal_at(&reference,"Replacement objective",103,"second".into())?;
    let archived:Goal=serde_json::from_str(std::fs::read_to_string(goal_history_file_path(&reference))?.trim())?;
    if archived!=fetched{return Err("archive QA mismatch".into());}
    println!("{}",serde_json::to_string_pretty(&goal_tool_response(Some(&fetched)))?);println!("archiveCount=1 replacementId=second");temp.close()?;println!("cleanup=tempdir removed");Ok(())
}
