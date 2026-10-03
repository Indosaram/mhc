use std::collections::BTreeSet;
use crate::{todo_widget::{TodoWidgetModel,TodoWidgetRow},todo_types::{TodoStatus,TodoCompletionTransition}};
pub fn theme(source:&maho_ext_api::Theme)->maho_interactive::theme::Theme {
    use maho_interactive::theme::{theme::ColorMode,theme_json::{ColorValue,ThemeJson}};
    let colors=source.colors.iter().chain(&source.backgrounds).map(|(key,prefix)| {
        let code=prefix.strip_prefix("\x1b[").and_then(|value|value.strip_suffix('m')).unwrap_or_else(||panic!("Expected exported ANSI theme prefix"));
        let fields=code.split(';').collect::<Vec<_>>();
        let value=match fields.as_slice() {
            ["39"|"49"]=>ColorValue::Text(String::new()),
            ["38"|"48","5",index]=>ColorValue::Index(index.parse().unwrap_or_else(|error|std::panic::panic_any(error))),
            ["38"|"48","2",red,green,blue]=>{
                let channels=[red,green,blue].map(|channel|channel.parse::<u8>().unwrap_or_else(|error|std::panic::panic_any(error)));
                ColorValue::Text(format!("#{:02x}{:02x}{:02x}",channels[0],channels[1],channels[2]))
            },
            _=>panic!("Unsupported exported ANSI theme prefix"),
        };
        (key.clone(),value)
    }).collect();
    let mode=if source.colors.values().chain(source.backgrounds.values()).any(|prefix|prefix.starts_with("\x1b[38;2;")||prefix.starts_with("\x1b[48;2;")){ColorMode::Truecolor}else{ColorMode::Color256};
    maho_interactive::theme::Theme::from_json(ThemeJson {name:source.name.clone().unwrap_or_default(),colors,vars:Default::default(),export_colors:Default::default()},mode).unwrap_or_else(|error|std::panic::panic_any(error))
}
pub fn format_task(task:&crate::todo_types::TodoItem,theme:&maho_interactive::theme::Theme,completed:bool,frame:Option<i64>)->String {
    use maho_interactive::{theme::ThemeColor,components::todo_strike::{strike_reveal_count,partial_strikethrough}};
    let status=match task.status {TodoStatus::Pending=>"pending",TodoStatus::InProgress=>"in_progress",TodoStatus::Completed=>"completed",TodoStatus::Abandoned=>"abandoned"};
    let line=format!("{} {}",crate::todo_format::get_todo_marker(status),crate::todo_format::sanitize_todo_text(&task.content));
    match task.status {
        TodoStatus::Completed=>{let reveal=if completed{strike_reveal_count(&line,frame)}else{None};theme.fg(ThemeColor::Dim,&reveal.map_or_else(||theme.strikethrough(&line),|count|partial_strikethrough(&line,count,|text|theme.strikethrough(text))))},
        TodoStatus::InProgress=>theme.fg(ThemeColor::Accent,&theme.bold(&line)),
        TodoStatus::Abandoned=>theme.fg(ThemeColor::Dim,&line),TodoStatus::Pending=>line,
    }
}
pub struct TodoWidgetComponent {model:TodoWidgetModel,theme:maho_interactive::theme::Theme,completion_keys:BTreeSet<String>,frame:Option<i64>,text:maho_tui::components::text::Text}
pub fn widget_content(phases:&[crate::todo_types::TodoPhase],completed:&[TodoCompletionTransition],sender:tokio::sync::mpsc::UnboundedSender<maho_interactive::interactive_extension_ui::UiRequest>)->Option<maho_ext_api::WidgetContent> {
    let model=crate::todo_widget::get_todo_widget_model(phases)?;let completed=completed.to_vec();
    Some(maho_interactive::widget_clock::widget_content(std::sync::Arc::new(move |theme|TodoWidgetComponent::new(theme,model.clone(),&completed)),TodoWidgetComponent::tick,sender))
}
impl TodoWidgetComponent {
    pub fn new(theme:&maho_ext_api::Theme,model:TodoWidgetModel,completed:&[TodoCompletionTransition])->Self {
        let completion_keys=get_animated_completion_keys(&model,completed);let frame=if completion_keys.is_empty(){None}else{Some(0)};
        Self {model,theme:self::theme(theme),completion_keys,frame,text:maho_tui::components::text::Text::with_padding("",1,0)}
    }
    /// Called by an interactive clock adapter at each source frame interval.
    pub fn tick(&mut self)->bool {
        let Some(frame)=self.frame else{return false;};
        self.frame=if frame+1>=maho_interactive::components::todo_strike::TODO_STRIKE_TOTAL_FRAMES{None}else{Some(frame+1)};true
    }
}
impl maho_ext_api::Component for TodoWidgetComponent {
    fn render(&mut self,width:usize)->Vec<String> {
        self.text.set_text(self.model.rows.iter().map(|row|match row {TodoWidgetRow::Label(text)=>text.clone(),TodoWidgetRow::Task(task)=>format_task(task,&self.theme,self.completion_keys.contains(&task.content),self.frame)}).collect::<Vec<_>>().join("\n"));
        self.text.render(width)
    }
    fn invalidate(&mut self){self.text.invalidate();}
    fn dispose(&mut self){self.frame=None;}
}
pub fn get_animated_completion_keys(model:&TodoWidgetModel,completed_tasks:&[TodoCompletionTransition])->BTreeSet<String> {
    let visible:BTreeSet<_>=model.rows.iter().filter_map(|row|match row { TodoWidgetRow::Task(task) if task.status==TodoStatus::Completed=>Some(task.content.as_str()),_=>None }).collect();
    completed_tasks.iter().filter(|transition|transition.phase==model.phase_name && visible.contains(transition.content.as_str())).map(|transition|transition.content.clone()).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn component_advances_bounded_completion_frames_and_disposes() {
        use maho_ext_api::Component;
        let model=TodoWidgetModel {phase_name:"Delivery".into(),rows:vec![TodoWidgetRow::Label("Delivery".into()),TodoWidgetRow::Task(crate::todo_types::TodoItem {content:"Finished".into(),status:TodoStatus::Completed}),TodoWidgetRow::Task(crate::todo_types::TodoItem {content:"Active".into(),status:TodoStatus::InProgress})]};
        let completed=[TodoCompletionTransition {phase:"Delivery".into(),content:"Finished".into()}];
        let theme=maho_ext_api::Theme::default();
        let mut component=TodoWidgetComponent::new(&theme,model.clone(),&completed);
        assert!(!component.render(80).join("\n").contains("\u{1b}[9m"));
        for _ in 0..8 {assert!(component.tick());}
        assert!(component.render(80).join("\n").contains("\u{1b}[9m"));
        for _ in 8..maho_interactive::components::todo_strike::TODO_STRIKE_TOTAL_FRAMES {assert!(component.tick());}
        assert!(!component.tick());
        let settled=component.render(80);
        assert!(settled.join("\n").contains("Finished"));
        let mut restored=TodoWidgetComponent::new(&theme,model.clone(),&[]);
        assert!(!restored.tick());
        let mut disposed=TodoWidgetComponent::new(&theme,model,&completed);
        disposed.dispose();assert!(!disposed.tick());
        for width in [40,80,120] {let lines=component.render(width);assert!(lines.iter().all(|line|maho_tui::utils::visible_width(line)<=width));println!("TODO_WIDGET_RENDER_JSON={}",serde_json::json!({"width":width,"states":[lines]}));}
    }
    #[test] fn animation_targets_only_visible_completed_tasks_in_current_phase() { let model=TodoWidgetModel{phase_name:"Current".into(),rows:vec![TodoWidgetRow::Task(crate::todo_types::TodoItem{content:"visible".into(),status:TodoStatus::Completed}),TodoWidgetRow::Task(crate::todo_types::TodoItem{content:"pending".into(),status:TodoStatus::Pending})]}; let transitions=[("Current","visible"),("Other","visible"),("Current","hidden"),("Current","pending")].map(|(phase,content)|TodoCompletionTransition{phase:phase.into(),content:content.into()}); assert_eq!(get_animated_completion_keys(&model,&transitions),BTreeSet::from(["visible".into()])); }
}
