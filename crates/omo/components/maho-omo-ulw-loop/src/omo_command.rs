use std::{collections::BTreeMap,path::Path,process::Stdio};

pub struct SpawnTarget { pub command:String,pub args:Vec<String> }
pub fn to_spawn_target(bin:&str,args:&[String],platform:&str,js_runtime:&str)->SpawnTarget {
    let lower=bin.to_ascii_lowercase();
    if lower.ends_with(".js") { return SpawnTarget{command:js_runtime.into(),args:std::iter::once(bin.into()).chain(args.iter().cloned()).collect()}; }
    if platform=="win32" && (lower.ends_with(".cmd")||lower.ends_with(".bat")) { return SpawnTarget{command:"cmd.exe".into(),args:["/d","/s","/c",bin].into_iter().map(str::to_owned).chain(args.iter().cloned()).collect()}; }
    SpawnTarget{command:bin.into(),args:args.to_vec()}
}
pub fn resolve_omo_bin(env:&BTreeMap<String,String>,path_lookup:impl Fn(&str)->Option<String>)->Option<String> {
    if let Some(bin)=env.get("OMO_AGENT_TOOLKIT_BIN").map(|s|s.trim()).filter(|s|!s.is_empty()) { return Some(bin.into()); }
    if let Some(bin)=path_lookup("omo-agent-toolkit") { return Some(bin); }
    env.get("OMO_BIN").map(|s|s.trim()).filter(|s|!s.is_empty()).map(str::to_owned)
}
pub struct CommandResult { pub code:i32,pub stdout:String }
pub async fn run_omo_command(target:&SpawnTarget,cwd:&Path)->CommandResult {
    use tokio::io::AsyncReadExt;
    let child=tokio::process::Command::new(&target.command).args(&target.args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn();
    let Ok(mut child)=child else { return CommandResult{code:1,stdout:String::new()}; };
    let mut bytes=Vec::new();
    let Some(mut stdout)=child.stdout.take() else { return CommandResult{code:1,stdout:String::new()}; };
    let result=tokio::time::timeout(std::time::Duration::from_secs(30),async {
        stdout.read_to_end(&mut bytes).await?;
        child.wait().await
    }).await;
    let code=match result { Ok(Ok(status))=>status.code().unwrap_or(1),Ok(Err(_))|Err(_)=>1 };
    if code==1 && let Err(error)=child.kill().await && error.kind()!=std::io::ErrorKind::InvalidInput { eprintln!("ulw-loop command cleanup failed: {error}"); }
    CommandResult{code,stdout:String::from_utf8_lossy(&bytes).into_owned()}
}
