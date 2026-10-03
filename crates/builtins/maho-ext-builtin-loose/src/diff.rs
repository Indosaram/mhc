#[derive(Clone,Debug, PartialEq, Eq)]
pub struct FileInfo { pub status: String, pub status_label: String, pub file: String }
pub fn parse_status(stdout: &str) -> Vec<FileInfo> {
    stdout.split('\n').filter_map(|line| {
        if line.encode_utf16().count() < 4 { return None; }
        let mut chars = line.chars();
        let status: String = chars.by_ref().take(2).collect();
        let file = chars.as_str().trim_start().to_owned();
        let label = ['M','A','D','?','R','C'].into_iter().find(|c| status.contains(*c)).map(|c| c.to_string()).unwrap_or_else(|| {
            let trimmed = status.trim();
            if trimmed.is_empty() { "~".into() } else { trimmed.into() }
        });
        Some(FileInfo { status: label.clone(), status_label: label, file })
    }).collect()
}
pub fn windows_unsafe_cmd_path(path: &str) -> bool { path.contains(['&','|','<','>','^','%','\r','\n']) }
pub fn quote_cmd_arg(value: &str) -> String { format!("\"{}\"", value.replace('"', "\"\"")) }

use maho_ext_api::*;
use std::sync::{Arc,Mutex};
pub struct Diff{pub render_binding:crate::files::RenderBinding}
impl Extension for Diff{
    fn register(&self,api:&mut ExtensionApi){
        let sender=Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));let render_binding=self.render_binding.clone();
        api.register_command("diff",Some("Show git changes and open in VS Code diff view".into()),None,Arc::new(move|_,ctx|{let sender=sender.clone();let render_binding=render_binding.clone();Box::pin(async move{
            if !ctx.has_ui{ctx.ui.notify("No UI available",NotificationType::Error);return Ok(());}
            let status=sender.exec("git",&["status".into(),"--porcelain".into()],ExecOptions{cwd:Some(ctx.cwd.clone()),..Default::default()}).await?;
            if status.code!=0{ctx.ui.notify(&format!("git status failed: {}",status.stderr),NotificationType::Error);return Ok(());}
            if status.stdout.trim().is_empty(){ctx.ui.notify("No changes in working tree",NotificationType::Info);return Ok(());}
            let files=parse_status(&status.stdout);if files.is_empty(){ctx.ui.notify("No changes found",NotificationType::Info);return Ok(());}
            let tasks=Arc::new(Mutex::new(Vec::<tokio::task::JoinHandle<()>>::new()));let pending=tasks.clone();let owner=ctx.clone();let runtime=tokio::runtime::Handle::current();
            let result=ctx.ui.custom_factory(Arc::new(move|host,theme,_,done|{
                let render=render_binding(host);let owner=owner.clone();let sender=sender.clone();let pending=pending.clone();let selected=files.clone();let runtime=runtime.clone();
                let items=files.iter().enumerate().map(|(index,file)|{let color=match file.status.as_str(){"M"=>"warning","A"=>"success","D"=>"error","?"=>"muted",_=>"dim"};maho_tui::components::select_list::SelectItem{value:index.to_string(),label:format!("{}{}\x1b[39m {}",theme.colors.get(color).map_or("\x1b[39m",String::as_str),file.status_label,file.file),description:None}}).collect();
                let open=Arc::new(move|index:usize|{if let Some(file)=selected.get(index){let file=file.clone();let sender=sender.clone();let owner=owner.clone();pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(runtime.spawn(async move{
                    let result=async{
                        if file.status!="?"{
                            let result=sender.exec("git",&["difftool".into(),"-y".into(),"--tool=vscode".into(),file.file.clone()],ExecOptions{cwd:Some(owner.cwd.clone()),..Default::default()}).await?;
                            if result.code==0{return Ok(());}
                            owner.ui.notify(&format!("Failed to show diff with vscode for {} (exit {}){}",file.file,result.code,if result.stderr.trim().is_empty(){String::new()}else{format!(": {}",result.stderr.trim())}),NotificationType::Error);
                            owner.ui.notify("Troubleshooting: check git difftool config (e.g. `git config --get difftool.vscode.cmd`).",NotificationType::Info);
                        }
                        crate::files::open_code(&sender,&owner,&file.file).await
                    }.await;
                    if let Err(error)=result{owner.ui.notify(&format!("Failed to open {}: {}",file.file,error.message),NotificationType::Error);}
                }));}});
                let panel=crate::files::FilePicker::new("Select file to diff",items,theme.clone(),render,done,open);Box::pin(async move{Ok(Box::new(panel) as Box<dyn Component>)})
            }),Default::default()).await;
            let tasks=std::mem::take(&mut *tasks.lock().unwrap_or_else(std::sync::PoisonError::into_inner));for task in tasks{let _result=task.await;}
            result?;Ok(())
        })}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_priority_and_rename_text_match_porcelain_policy() {
        let files = parse_status("AM file\n?? new\n R old -> new\n!! ignored\nx\n");
        assert_eq!(files[0].status, "M");
        assert_eq!(files[1].status, "?");
        assert_eq!(files[2].file, "old -> new");
        assert_eq!(files[3].status, "!!");
        assert_eq!(files.len(), 4);
    }
    #[test]
    fn cmd_quoting_and_metacharacters_follow_upstream() {
        assert_eq!(quote_cmd_arg("a\"b"), "\"a\"\"b\"");
        assert!(windows_unsafe_cmd_path("a%b"));
        assert!(!windows_unsafe_cmd_path("a b"));
    }
}
