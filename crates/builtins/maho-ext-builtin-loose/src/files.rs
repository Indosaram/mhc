use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub operations: BTreeSet<String>,
    pub last_timestamp: i64,
}

pub fn collect_files(branch: &[Value]) -> Vec<FileEntry> {
    let mut calls = BTreeMap::new();
    for entry in branch {
        if entry.get("type").and_then(Value::as_str) != Some("message") { continue; }
        let message = &entry["message"];
        if message.get("role").and_then(Value::as_str) != Some("assistant") { continue; }
        if let Some(content) = message.get("content").and_then(Value::as_array) {
            for block in content {
                if block.get("type").and_then(Value::as_str) != Some("toolCall") { continue; }
                let Some(name) = block.get("name").and_then(Value::as_str) else { continue; };
                let (paths, operation) = match name {
                    "read" | "write" | "edit" => {
                        let Some(path) = block["arguments"].get("path").and_then(Value::as_str).filter(|path| !path.is_empty()) else { continue; };
                        (vec![path.to_owned()], name)
                    }
                    "apply_patch" => {
                        let Some(input) = block["arguments"].get("input").and_then(Value::as_str) else { continue; };
                        (maho_ext_gpt_apply_patch::text::extract_patched_paths(input), "edit")
                    }
                    _ => continue,
                };
                if let Some(id) = block.get("id").and_then(Value::as_str) {
                    calls.insert(id.to_owned(), (paths, operation.to_owned()));
                }
            }
        }
    }
    let mut files = Vec::<FileEntry>::new();
    for entry in branch {
        if entry.get("type").and_then(Value::as_str) != Some("message") { continue; }
        let message = &entry["message"];
        if message.get("role").and_then(Value::as_str) != Some("toolResult") { continue; }
        let Some((paths, name)) = message.get("toolCallId").and_then(Value::as_str).and_then(|id| calls.get(id)) else { continue; };
        let timestamp = message.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
        for path in paths {
            if let Some(existing) = files.iter_mut().find(|file| file.path == *path) {
                existing.operations.insert(name.clone());
                existing.last_timestamp = existing.last_timestamp.max(timestamp);
            } else {
                files.push(FileEntry { path: path.clone(), operations: BTreeSet::from([name.clone()]), last_timestamp: timestamp });
            }
        }
    }
    files.sort_by_key(|file| std::cmp::Reverse(file.last_timestamp));
    files
}

use maho_ext_api::*;
use maho_tui::{components::{select_list::{SelectList,SelectItem,SelectListTheme,SelectListLayoutOptions},text::Text},tui::Component};
use std::{rc::Rc,cell::Cell,sync::Arc};
pub type RenderBinding=Arc<dyn Fn(&dyn ExtensionTuiHost)->Rc<dyn Fn()>+Send+Sync>;
pub struct FilePicker{list:SelectList,title:Text,help:Text,theme:Theme,index:Rc<Cell<usize>>,count:usize,rows:usize,render:Rc<dyn Fn()>}
impl FilePicker{
    pub fn new(title:&str,items:Vec<SelectItem>,theme:Theme,render:Rc<dyn Fn()>,done:CustomUiDone,open:Arc<dyn Fn(usize)+Send+Sync>)->Self{
        let color=|key:&str|{let color=theme.colors.get(key).cloned().unwrap_or_else(||"\x1b[39m".into());Rc::new(move|text:&str|format!("{color}{text}\x1b[39m")) as Rc<dyn Fn(&str)->String>};
        let count=items.len();let rows=count.clamp(1,15);let index=Rc::new(Cell::new(0));
        let mut list=SelectList::new(items,rows,SelectListTheme{selected_prefix:color("accent"),selected_text:Rc::new(str::to_owned),description:color("muted"),scroll_info:color("dim"),no_match:color("warning"),render_row:None},SelectListLayoutOptions::default());
        list.on_select=Some(Box::new(move|item|{if let Ok(index)=item.value.parse::<usize>(){open(index);}}));
        list.on_cancel=Some(Box::new(move||done(Value::Null)));
        let selected=index.clone();list.on_selection_change=Some(Box::new(move|item|{if let Ok(index)=item.value.parse::<usize>(){selected.set(index);}}));
        let title=Text::with_padding(color("accent")(&format!("\x1b[1m {title}\x1b[22m")),0,0);
        let help=Text::with_padding(color("dim")(" ↑↓ navigate • ←→ page • enter open • esc close"),0,0);
        Self{list,title,help,theme,index,count,rows,render}
    }
}
impl Component for FilePicker{
    fn render(&mut self,width:usize)->Vec<String>{let border=format!("{}{}\x1b[39m",self.theme.colors.get("accent").map_or("\x1b[39m",String::as_str),"─".repeat(width.max(1)));[vec![border.clone()],self.title.render(width),self.list.render(width),self.help.render(width),vec![border]].concat()}
    fn handle_input(&mut self,data:&str){
        if maho_tui::keys::matches_key(data,"left"){self.index.set(self.index.get().saturating_sub(self.rows));self.list.set_selected_index(self.index.get());}
        else if maho_tui::keys::matches_key(data,"right"){self.index.set((self.index.get()+self.rows).min(self.count.saturating_sub(1)));self.list.set_selected_index(self.index.get());}
        else{self.list.handle_input(data);}(self.render)();
    }
    fn has_input_handler(&self)->bool{true}
    fn invalidate(&mut self){self.title.invalidate();self.list.invalidate();self.help.invalidate();}
}
pub(crate) async fn open_code(sender:&ExtensionApi,ctx:&ExtensionContext,path:&str)->Result<(),ExtensionFailure>{
    let (command,args)=if cfg!(windows){
        if crate::diff::windows_unsafe_cmd_path(path){ctx.ui.notify(&format!("Refusing to open {path}: path contains Windows cmd metacharacters (& | < > ^ % or newline)."),NotificationType::Error);return Ok(());}
        ("cmd",vec!["/d".into(),"/s".into(),"/c".into(),format!("code -g {}",crate::diff::quote_cmd_arg(path))])
    }else{("code",vec!["-g".into(),path.into()])};
    let result=sender.exec(command,&args,ExecOptions{cwd:Some(ctx.cwd.clone()),..Default::default()}).await?;
    if result.code!=0{ctx.ui.notify(&format!("Failed to open {path} (exit {}){}",result.code,if result.stderr.trim().is_empty(){String::new()}else{format!(": {}",result.stderr.trim())}),NotificationType::Error);}
    Ok(())
}
pub struct Files{pub render_binding:RenderBinding}
impl Extension for Files{
    fn register(&self,api:&mut ExtensionApi){
        let sender=Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));let render_binding=self.render_binding.clone();
        api.register_command("files",Some("Show files read/written/edited in this session".into()),None,Arc::new(move|_,ctx|{let sender=sender.clone();let render_binding=render_binding.clone();Box::pin(async move{
            if !ctx.has_ui{ctx.ui.notify("No UI available",NotificationType::Error);return Ok(());}
            let files=collect_files(&ctx.session_manager.get_branch().into_iter().map(|entry|entry.data).collect::<Vec<_>>());
            if files.is_empty(){ctx.ui.notify("No files read/written/edited in this session",NotificationType::Info);return Ok(());}
            let tasks=Arc::new(std::sync::Mutex::new(Vec::<tokio::task::JoinHandle<()>>::new()));let pending=tasks.clone();let owner=ctx.clone();
            let runtime=tokio::runtime::Handle::current();
            let result=ctx.ui.custom_factory(Arc::new(move|host,theme,_,done|{
                let render=render_binding(host);let owner=owner.clone();let sender=sender.clone();let pending=pending.clone();let selected=files.clone();
                let items=files.iter().enumerate().map(|(index,file)|SelectItem{value:index.to_string(),label:format!("{} {}",["read","write","edit"].iter().filter(|operation|file.operations.contains(**operation)).map(|operation|{let (label,color)=match *operation{"read"=>("R","muted"),"write"=>("W","success"),_=>("E","warning")};format!("{}{label}\x1b[39m",theme.colors.get(color).map_or("\x1b[39m",String::as_str))}).collect::<String>(),file.path),description:None}).collect();
                let runtime=runtime.clone();
                let open=Arc::new(move|index:usize|{if let Some(file)=selected.get(index){let path=file.path.clone();let sender=sender.clone();let owner=owner.clone();pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(runtime.spawn(async move{if let Err(error)=open_code(&sender,&owner,&path).await{owner.ui.notify(&format!("Failed to open {path}: {}",error.message),NotificationType::Error);}}));}});
                let panel=FilePicker::new("Select file to open",items,theme.clone(),render,done,open);Box::pin(async move{Ok(Box::new(panel) as Box<dyn Component>)})
            }),Default::default()).await;
            let tasks=std::mem::take(&mut *tasks.lock().unwrap_or_else(std::sync::PoisonError::into_inner));for task in tasks{let _result=task.await;}
            result?;Ok(())
        })}));
    }
}
