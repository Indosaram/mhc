use crate::{filter::filter_history,types::HistoryEntry};
use maho_tui::{components::{input::{Input,InputOptions},select_list::{SelectList,SelectItem,SelectListTheme,SelectListLayoutOptions}},tui::{Component,Focusable},keybindings::get_keybindings};
use maho_interactive::theme::{Theme,ThemeColor};
use std::{rc::Rc,path::Path};

pub fn relative_time(timestamp:i64,now:i64)->String{
    let seconds=now.saturating_sub(timestamp).max(0)/1000;
    if seconds<60{return "now".into();}let minutes=seconds/60;
    if minutes<60{return format!("{minutes}m ago");}let hours=minutes/60;
    if hours<24{return format!("{hours}h ago");}let days=hours/24;
    if days<30{return format!("{days}d ago");}let months=days/30;
    if months<12{return format!("{months}mo ago");}format!("{}y ago",months/12)
}
pub struct HistorySearchOverlay{
    input:Input,entries:Vec<HistoryEntry>,filtered:Vec<HistoryEntry>,list:SelectList,theme:Theme,
    render_request:Rc<dyn Fn()>,done:Rc<dyn Fn(Option<HistoryEntry>)>,focused:bool,
}
impl HistorySearchOverlay{
    pub fn new(entries:Vec<HistoryEntry>,theme:Theme,render_request:Rc<dyn Fn()>,done:Rc<dyn Fn(Option<HistoryEntry>)>)->Self{
        let list=Self::build_list(&entries,&theme,Rc::clone(&done));
        Self{input:Input::new(InputOptions::default()),filtered:entries.clone(),entries,list,theme,render_request,done,focused:false}
    }
    fn build_list(entries:&[HistoryEntry],theme:&Theme,done:Rc<dyn Fn(Option<HistoryEntry>)>)->SelectList{
        let now=chrono::Utc::now().timestamp_millis();
        let items=entries.iter().take(250).enumerate().map(|(index,entry)|{
            let short:String=entry.session_id.chars().take(8).collect();let cwd=Path::new(&entry.cwd).file_name().unwrap_or_default().to_string_lossy();
            let label=entry.text.split(['\r','\n']).filter(|part|!part.is_empty()).collect::<Vec<_>>().join(" ").trim().to_owned();
            SelectItem{value:index.to_string(),label,description:Some(format!("{} · {}",if cwd.is_empty(){short}else{format!("{cwd}/{short}")},relative_time(entry.timestamp,now)))}
        }).collect::<Vec<_>>();
        let accent=theme.clone();let muted=theme.clone();let dim=theme.clone();let warning=theme.clone();
        let mut list=SelectList::new(items,entries.len().clamp(1,15),SelectListTheme{
            selected_prefix:Rc::new(move|text|accent.fg(ThemeColor::Accent,text)),selected_text:Rc::new(str::to_owned),
            description:Rc::new(move|text|muted.fg(ThemeColor::Muted,text)),scroll_info:Rc::new(move|text|dim.fg(ThemeColor::Dim,text)),
            no_match:Rc::new(move|text|warning.fg(ThemeColor::Warning,&text.replace("commands","prompts"))),render_row:None,
        },SelectListLayoutOptions::default());
        let selected=entries.to_vec();let on_select=Rc::clone(&done);
        list.on_select=Some(Box::new(move|item|on_select(item.value.parse::<usize>().ok().and_then(|index|selected.get(index)).cloned())));
        list.on_cancel=Some(Box::new(move||done(None)));list
    }
    pub fn search_value(&self)->&str{self.input.get_value()}
    pub fn filtered_entries(&self)->&[HistoryEntry]{&self.filtered}
}
impl Focusable for HistorySearchOverlay{
    fn focused(&self)->bool{self.focused}
    fn set_focused(&mut self,value:bool){self.focused=value;self.input.set_focused(value);}
}
impl Component for HistorySearchOverlay{
    fn render(&mut self,width:usize)->Vec<String>{
        let border=self.theme.fg(ThemeColor::Accent,&"─".repeat(width.max(1)));
        let mut lines=vec![border.clone(),format!("{}{}",self.theme.fg(ThemeColor::Accent,&self.theme.bold(" Search prompt history")),self.theme.fg(ThemeColor::Dim,&format!(" {}/{} prompts",self.filtered.len(),self.entries.len())))];
        lines.extend(self.input.render(width));lines.extend(self.list.render(width));
        lines.push(self.theme.fg(ThemeColor::Dim," Type to filter • ↑↓ navigate • enter select • esc close"));lines.push(border);lines
    }
    fn handle_input(&mut self,data:&str){
        let keys=get_keybindings();if ["tui.select.up","tui.select.down","tui.select.confirm","tui.select.cancel"].iter().any(|action|keys.matches(data,action)){self.list.handle_input(data);return;}
        let before=self.input.get_value().to_owned();self.input.handle_input(data);
        if before!=self.input.get_value(){self.filtered=filter_history(&self.entries,self.input.get_value());self.list=Self::build_list(&self.filtered,&self.theme,Rc::clone(&self.done));(self.render_request)();}
    }
    fn has_input_handler(&self)->bool{true}
    fn invalidate(&mut self){self.input.invalidate();self.list.invalidate();}
    fn focusable_get(&self)->Option<bool>{Some(self.focused)}
    fn focusable_set(&mut self,value:bool){self.set_focused(value);}
}
