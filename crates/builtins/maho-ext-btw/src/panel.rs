use maho_ext_api::{Theme,WidgetContent};
use maho_interactive::components::dynamic_border::DynamicBorder;
use maho_tui::{components::{markdown::{Markdown,MarkdownOptions,MarkdownTheme,ThemeFn},text::Text},tui::Component};
use std::{rc::Rc,sync::{Arc,Mutex,atomic::{AtomicU64,Ordering}}};
pub enum BtwPanelStatus{Streaming,Done,Error(String),Aborted}
pub struct PanelState{pub answer:String,pub status:BtwPanelStatus}
pub struct BtwPanel{border:DynamicBorder,header:Text,body:Markdown,footer:Text,question:String,state:Arc<Mutex<PanelState>>,theme:Theme}
impl BtwPanel{
    pub fn new(question:&str,state:Arc<Mutex<PanelState>>,theme:Theme)->Self{
        let fg=|color:&str,text:&str|format!("{}{text}\x1b[39m",theme.colors.get(color).map_or("\x1b[39m",String::as_str));
        let header=fg("accent","\x1b[1mbtw: \x1b[22m")+&fg("text",question);
        let footer=match &BtwPanelStatus::Streaming{
            BtwPanelStatus::Streaming=>fg("dim","\nanswering… (/btw or Esc to cancel)"),
            BtwPanelStatus::Done=>fg("dim","\n(/btw or Esc to dismiss; clears on next message)"),
            BtwPanelStatus::Error(detail)=>fg("error",&format!("\nerror: {detail}")),
            BtwPanelStatus::Aborted=>fg("dim","\n(dismissed)"),
        };
        let border_theme=theme.clone();
        let fg=|key:&str|{let color=theme.clone();let key=key.to_owned();Arc::new(move|text:&str|color.fg(&key,text)) as ThemeFn};
        let bold=Arc::new(|text:&str|format!("\x1b[1m{text}\x1b[22m")) as ThemeFn;
        let italic=Arc::new(|text:&str|format!("\x1b[3m{text}\x1b[23m")) as ThemeFn;
        let underline=Arc::new(|text:&str|format!("\x1b[4m{text}\x1b[24m")) as ThemeFn;
        let strikethrough=Arc::new(|text:&str|format!("\x1b[9m{text}\x1b[29m")) as ThemeFn;
        static NEXT_MARKDOWN_THEME_ID:AtomicU64=AtomicU64::new(0);
        let markdown_theme=MarkdownTheme{id:NEXT_MARKDOWN_THEME_ID.fetch_add(1,Ordering::Relaxed),heading:fg("mdHeading"),link:fg("mdLink"),link_url:fg("mdLinkUrl"),code:fg("mdCode"),code_block:fg("mdCodeBlock"),code_block_border:fg("mdCodeBlockBorder"),quote:fg("mdQuote"),quote_border:fg("mdQuoteBorder"),hr:fg("mdHr"),list_bullet:fg("mdListBullet"),bold,italic,strikethrough,underline,highlight_code:None,code_block_indent:None};
        Self{border:DynamicBorder::with_color(Rc::new(move|text|format!("{}{text}\x1b[39m",border_theme.colors.get("muted").map_or("\x1b[39m",String::as_str)))),header:Text::with_padding(header,1,0),body:Markdown::new("",1,0,markdown_theme,None,MarkdownOptions::default()),footer:Text::with_padding(footer,1,0),question:question.into(),state,theme}
    }
}
impl Component for BtwPanel{
    fn render(&mut self,width:usize)->Vec<String>{
        let state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let fg=|color:&str,text:&str|format!("{}{text}\x1b[39m",self.theme.colors.get(color).map_or("\x1b[39m",String::as_str));
        let footer=match &state.status{BtwPanelStatus::Streaming=>fg("dim","\nanswering… (/btw or Esc to cancel)"),BtwPanelStatus::Done=>fg("dim","\n(/btw or Esc to dismiss; clears on next message)"),BtwPanelStatus::Error(detail)=>fg("error",&format!("\nerror: {detail}")),BtwPanelStatus::Aborted=>fg("dim","\n(dismissed)")};
        self.header.set_text(fg("accent","\x1b[1mbtw: \x1b[22m")+&fg("text",&self.question));
        self.body.set_text(state.answer.as_str());
        self.footer.set_text(footer);
        [self.border.render(width),self.header.render(width),self.body.render(width),self.footer.render(width),self.border.render(width)].concat()
    }
    fn invalidate(&mut self){self.border.invalidate();self.header.invalidate();self.body.invalidate();self.footer.invalidate();}
}
pub fn widget(question:&str,state:Arc<Mutex<PanelState>>)->WidgetContent{
    let question=question.to_owned();
    WidgetContent::Component(Arc::new(move|theme|Box::new(BtwPanel::new(&question,state.clone(),theme.clone()))))
}

#[cfg(test)]
mod tests{
    use super::*;

    #[test]
    fn answer_renders_markdown_instead_of_literal_markup(){
        let state=Arc::new(Mutex::new(PanelState{answer:"**bold**".into(),status:BtwPanelStatus::Done}));
        let mut panel=BtwPanel::new("question",state,Theme::default());
        let rendered=panel.render(40).join("\n");
        assert!(!rendered.contains("**bold**"));
        assert!(rendered.contains("\x1b[1mbold\x1b[22m"));
    }
}
