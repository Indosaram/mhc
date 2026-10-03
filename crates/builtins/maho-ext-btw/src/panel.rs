use maho_ext_api::{Theme,WidgetContent};
use maho_interactive::{components::dynamic_border::DynamicBorder,theme::ThemeColor};
use maho_tui::{components::text::Text,tui::Component};
use std::{rc::Rc,sync::Arc};
pub enum BtwPanelStatus{Streaming,Done,Error(String),Aborted}
pub struct BtwPanel{border:DynamicBorder,body:Text}
impl BtwPanel{
    pub fn new(question:&str,answer:&str,status:&BtwPanelStatus,theme:Theme)->Self{
        let header=theme.fg(ThemeColor::Accent,&theme.bold("btw: "))+&theme.fg(ThemeColor::Text,question);
        let answer=if answer.is_empty(){String::new()}else{format!("\n{answer}")};
        let footer=match status{
            BtwPanelStatus::Streaming=>theme.fg(ThemeColor::Dim,"\nanswering… (/btw or Esc to cancel)"),
            BtwPanelStatus::Done=>theme.fg(ThemeColor::Dim,"\n(/btw or Esc to dismiss; clears on next message)"),
            BtwPanelStatus::Error(detail)=>theme.fg(ThemeColor::Error,&format!("\nerror: {detail}")),
            BtwPanelStatus::Aborted=>theme.fg(ThemeColor::Dim,"\n(dismissed)"),
        };
        Self{border:DynamicBorder::with_color(Rc::new(move|text|theme.fg(ThemeColor::Muted,text))),body:Text::with_padding(header+&answer+&footer,1,0)}
    }
}
impl Component for BtwPanel{
    fn render(&mut self,width:usize)->Vec<String>{[self.border.render(width),self.body.render(width),self.border.render(width)].concat()}
    fn invalidate(&mut self){self.border.invalidate();self.body.invalidate();}
}
pub fn widget(question:&str,answer:&str,done:bool)->WidgetContent{
    let question=question.to_owned();let answer=answer.to_owned();
    WidgetContent::Component(Arc::new(move|theme|Box::new(BtwPanel::new(&question,&answer,&if done{BtwPanelStatus::Done}else{BtwPanelStatus::Streaming},theme.clone()))))
}
