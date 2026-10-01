use memory_core::git::{GitMemoryRepo,errors::GitError};
use crate::context::MemoryIdentityContext;
pub const MEMORY_STATUS_KEY:&str="memory";
pub const MEMORY_STATUS_MAX_WIDTH:usize=60;
pub const MEMORY_HEALTH_SCAN_LIMIT:usize=20;
pub trait MemoryStatusUi{fn set_status(&mut self,key:&str,text:Option<&str>);fn notify(&mut self,message:&str,level:&str);}
#[derive(Default)]
pub struct MemoryStatusSegments{pub dirty:bool,pub backlog_steps:f64,pub streak:usize}
#[derive(Debug,PartialEq,Eq)]
pub struct MemoryStatusResult{pub notified:bool,pub footer_shown:bool}
pub struct RefreshMemoryStatusInput<'a>{pub context:&'a MemoryIdentityContext,pub compile_warn_tokens:usize,pub already_notified:bool,pub now_ms:i64,pub show_footer:bool,pub check_advisory:bool,pub session_id:Option<&'a str>}
pub fn format_relative_age(committed_at:f64,now:f64)->Option<String>{let age=now-committed_at;if !age.is_finite()||age<0.0{return None;}Some(if age<60_000.0{"just now".into()}else if age<3_600_000.0{format!("{}m ago",(age/60_000.0).floor())}else if age<86_400_000.0{format!("{}h ago",(age/3_600_000.0).floor())}else{format!("{}d ago",(age/86_400_000.0).floor())})}
pub fn format_memory_status_line(identity:&str,age:&str,segments:&MemoryStatusSegments)->String{let base=format!("mem:{identity} {age}");let optional=[if segments.dirty{"*".to_owned()}else{String::new()},if segments.backlog_steps>=1.0{format!(" (+{})",segments.backlog_steps)}else{String::new()},if segments.streak>=3{format!(" !{}",segments.streak)}else{String::new()}];for keep in (1..=optional.len()).rev(){let candidate=format!("{base}{}",optional[..keep].join(""));if candidate.encode_utf16().count()<=MEMORY_STATUS_MAX_WIDTH{return candidate;}}base}
pub fn read_memory_status_segments(repo:&GitMemoryRepo,context:&MemoryIdentityContext,session_id:Option<&str>,now:i64)->MemoryStatusSegments{
    let dirty=repo.status(&[] as &[&str]).is_ok_and(|status|!status.trim().is_empty());
    let backlog_steps=session_id.filter(|id|!id.is_empty()).and_then(|id|std::fs::read(context.identity_paths.transcripts.join(id).join("state.json")).ok()).and_then(|bytes|serde_json::from_slice::<serde_json::Value>(&bytes).ok()).and_then(|value|value.get("steps_since_last_successful_reflection").and_then(serde_json::Value::as_f64)).filter(|steps|steps.is_finite()&&*steps>0.0).map(f64::floor).unwrap_or(0.0);
    let streak=crate::worker::health::read_reflection_health(&context.identity_paths.reflection.join("completions"),MEMORY_HEALTH_SCAN_LIMIT,now).streak;
    MemoryStatusSegments{dirty,backlog_steps,streak}
}
pub fn refresh_memory_status(repo:&GitMemoryRepo,ui:&mut dyn MemoryStatusUi,input:&RefreshMemoryStatusInput<'_>)->Result<MemoryStatusResult,GitError>{
    let Some(head)=repo.head()?else{return Ok(MemoryStatusResult{notified:false,footer_shown:false});};let mut footer_shown=false;
    if input.show_footer&&let Some(committed)=repo.head_commit_timestamp()?&&let Some(age)=format_relative_age(committed.to_string().parse::<f64>().unwrap_or(f64::NAN)*1000.0,input.now_ms.to_string().parse().unwrap_or(f64::NAN)){let segments=read_memory_status_segments(repo,input.context,input.session_id,input.now_ms);ui.set_status(MEMORY_STATUS_KEY,Some(&format_memory_status_line(&input.context.identity,&age,&segments)));footer_shown=true;}
    if !input.check_advisory||input.already_notified{return Ok(MemoryStatusResult{notified:false,footer_shown});}
    let paths=repo.ls_tree(Some(&head),None)?;let mut total_bytes=0;for path in paths.iter().filter(|path|path.starts_with("system/")&&path.ends_with(".md")){total_bytes+=repo.show(&head,path)?.len();}let estimate=total_bytes/4;
    if estimate<input.compile_warn_tokens{return Ok(MemoryStatusResult{notified:false,footer_shown});}
    ui.notify(&format!("system memory ~{estimate} tokens exceeds advisory {}; consider /doctor",input.compile_warn_tokens),"warning");Ok(MemoryStatusResult{notified:true,footer_shown})
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn age_boundaries(){for (age,text) in [(0.0,"just now"),(59_999.0,"just now"),(60_000.0,"1m ago"),(3_600_000.0,"1h ago"),(86_400_000.0,"1d ago")]{assert_eq!(format_relative_age(0.0,age).as_deref(),Some(text));}assert_eq!(format_relative_age(1.0,0.0),None);assert_eq!(format_relative_age(f64::NAN,0.0),None);}
    #[test]fn segments_drop_right_to_left_with_utf16_width(){let segments=MemoryStatusSegments{dirty:true,backlog_steps:12.0,streak:3};assert_eq!(format_memory_status_line("agent","1m ago",&segments),"mem:agent 1m ago* (+12) !3");let id="a".repeat(42);assert_eq!(format_memory_status_line(&id,"1m ago",&segments),format!("mem:{id} 1m ago* (+12)"));let id="😀".repeat(24);assert_eq!(format_memory_status_line(&id,"1m ago",&segments),format!("mem:{id} 1m ago*"));}
    #[derive(Default)]struct Ui{lines:Vec<String>,notifications:usize}
    impl MemoryStatusUi for Ui{fn set_status(&mut self,key:&str,text:Option<&str>){assert_eq!(key,MEMORY_STATUS_KEY);self.lines.push(text.unwrap_or("").into());}fn notify(&mut self,_:&str,level:&str){assert_eq!(level,"warning");self.notifications+=1;}}
    #[test]fn real_repo_footer_and_advisory_once(){let dir=tempfile::tempdir().unwrap();let paths=memory_core::identity::layout::build_identity_paths(dir.path(),"agent");let engine=crate::engine_session::prepare_memory_engine_session("agent",&paths,Default::default()).unwrap();let context=MemoryIdentityContext::new("agent".into(),paths,crate::binding::MemorySessionBinding{identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:1.0});let now=engine.repo.head_commit_timestamp().unwrap().unwrap()*1000;let mut ui=Ui::default();let mut input=RefreshMemoryStatusInput{context:&context,compile_warn_tokens:1,already_notified:false,now_ms:now,show_footer:true,check_advisory:true,session_id:None};assert_eq!(refresh_memory_status(&engine.repo,&mut ui,&input).unwrap(),MemoryStatusResult{notified:true,footer_shown:true});input.already_notified=true;assert!(!refresh_memory_status(&engine.repo,&mut ui,&input).unwrap().notified);assert_eq!(ui.notifications,1);assert_eq!(ui.lines,["mem:agent just now","mem:agent just now"]);}
}
