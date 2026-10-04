use maho_ext_api::{Theme,WidgetContent};
use maho_interactive::components::dynamic_border::DynamicBorder;
use maho_tui::{components::text::Text,tui::Component};
use std::{rc::Rc,sync::Arc};
pub enum BtwPanelStatus{Streaming,Done,Error(String),Aborted}
pub struct BtwPanel{border:DynamicBorder,body:Text}
impl BtwPanel{
    pub fn new(question:&str,answer:&str,status:&BtwPanelStatus,theme:Theme)->Self{
        let fg=|color:&str,text:&str|format!("{}{text}\x1b[39m",theme.colors.get(color).map_or("\x1b[39m",String::as_str));
        let header=fg("accent","\x1b[1mbtw: \x1b[22m")+&fg("text",question);
        let answer=if answer.is_empty(){String::new()}else{format!("\n{answer}")};
        let footer=match status{
            BtwPanelStatus::Streaming=>fg("dim","\nanswering… (/btw or Esc to cancel)"),
            BtwPanelStatus::Done=>fg("dim","\n(/btw or Esc to dismiss; clears on next message)"),
            BtwPanelStatus::Error(detail)=>fg("error",&format!("\nerror: {detail}")),
            BtwPanelStatus::Aborted=>fg("dim","\n(dismissed)"),
        };
        Self{border:DynamicBorder::with_color(Rc::new(move|text|format!("{}{text}\x1b[39m",theme.colors.get("muted").map_or("\x1b[39m",String::as_str)))),body:Text::with_padding(header+&answer+&footer,1,0)}
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
pub fn error_widget(question:&str,message:&str)->WidgetContent{
    let question=question.to_owned();let message=message.to_owned();
    WidgetContent::Component(Arc::new(move|theme|Box::new(BtwPanel::new(&question,"",&BtwPanelStatus::Error(message.clone()),theme.clone()))))
}
