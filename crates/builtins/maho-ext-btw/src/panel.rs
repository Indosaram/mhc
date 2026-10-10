use maho_ext_api::{Theme,WidgetContent};
use maho_interactive::components::dynamic_border::DynamicBorder;
use maho_tui::{components::text::Text,tui::Component};
use std::{rc::Rc,sync::{Arc,Mutex}};
pub enum BtwPanelStatus{Streaming,Done,Error(String),Aborted}
pub struct PanelState{pub answer:String,pub status:BtwPanelStatus}
pub struct BtwPanel{border:DynamicBorder,body:Text,question:String,state:Arc<Mutex<PanelState>>,theme:Theme}
impl BtwPanel{
    pub fn new(question:&str,state:Arc<Mutex<PanelState>>,theme:Theme)->Self{
        let fg=|color:&str,text:&str|format!("{}{text}\x1b[39m",theme.colors.get(color).map_or("\x1b[39m",String::as_str));
        let header=fg("accent","\x1b[1mbtw: \x1b[22m")+&fg("text",question);
        let answer=String::new();
        let footer=match &BtwPanelStatus::Streaming{
            BtwPanelStatus::Streaming=>fg("dim","\nanswering… (/btw or Esc to cancel)"),
            BtwPanelStatus::Done=>fg("dim","\n(/btw or Esc to dismiss; clears on next message)"),
            BtwPanelStatus::Error(detail)=>fg("error",&format!("\nerror: {detail}")),
            BtwPanelStatus::Aborted=>fg("dim","\n(dismissed)"),
        };
        let border_theme=theme.clone();
        Self{border:DynamicBorder::with_color(Rc::new(move|text|format!("{}{text}\x1b[39m",border_theme.colors.get("muted").map_or("\x1b[39m",String::as_str)))),body:Text::with_padding(header+&answer+&footer,1,0),question:question.into(),state,theme}
    }
}
impl Component for BtwPanel{
    fn render(&mut self,width:usize)->Vec<String>{
        let state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let fg=|color:&str,text:&str|format!("{}{text}\x1b[39m",self.theme.colors.get(color).map_or("\x1b[39m",String::as_str));
        let answer=if state.answer.is_empty(){String::new()}else{format!("\n{}",state.answer)};
        let footer=match &state.status{BtwPanelStatus::Streaming=>fg("dim","\nanswering… (/btw or Esc to cancel)"),BtwPanelStatus::Done=>fg("dim","\n(/btw or Esc to dismiss; clears on next message)"),BtwPanelStatus::Error(detail)=>fg("error",&format!("\nerror: {detail}")),BtwPanelStatus::Aborted=>fg("dim","\n(dismissed)")};
        self.body.set_text(format!("{}{}{}",fg("accent","\x1b[1mbtw: \x1b[22m")+&fg("text",&self.question),answer,footer));
        [self.border.render(width),self.body.render(width),self.border.render(width)].concat()
    }
    fn invalidate(&mut self){self.border.invalidate();self.body.invalidate();}
}
pub fn widget(question:&str,state:Arc<Mutex<PanelState>>)->WidgetContent{
    let question=question.to_owned();
    WidgetContent::Component(Arc::new(move|theme|Box::new(BtwPanel::new(&question,state.clone(),theme.clone()))))
}
