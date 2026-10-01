use maho_tui::{components::markdown::{Markdown,MarkdownTheme,MarkdownOptions,ThemeFn},tui::{Component,Focusable},keybindings::get_keybindings};
use maho_interactive::theme::{Theme,ThemeColor};
use std::{rc::Rc,sync::Arc};
pub const HELP_OVERLAY_MARGIN:usize=2;
pub struct HelpPanel{markdown:Markdown,rows:Rc<dyn Fn()->usize>,render_request:Rc<dyn Fn()>,done:Rc<dyn Fn()>,offset:usize,line_count:usize,focused:bool}
fn markdown_theme(theme:Theme)->MarkdownTheme{
    let color=|color|{let theme=theme.clone();Arc::new(move|text:&str|theme.fg(color,text)) as ThemeFn};
    let bold=theme.clone();let italic=theme.clone();let strike=theme.clone();let underline=theme.clone();
    MarkdownTheme{id:0,heading:color(ThemeColor::MdHeading),link:color(ThemeColor::MdLink),link_url:color(ThemeColor::MdLinkUrl),code:color(ThemeColor::MdCode),code_block:color(ThemeColor::MdCodeBlock),code_block_border:color(ThemeColor::MdCodeBlockBorder),quote:color(ThemeColor::MdQuote),quote_border:color(ThemeColor::MdQuoteBorder),hr:color(ThemeColor::MdHr),list_bullet:color(ThemeColor::MdListBullet),bold:Arc::new(move|text|bold.bold(text)),italic:Arc::new(move|text|italic.italic(text)),strikethrough:Arc::new(move|text|strike.strikethrough(text)),underline:Arc::new(move|text|underline.underline(text)),highlight_code:None,code_block_indent:None}
}
impl HelpPanel{
    pub fn new(markdown:&str,theme:Theme,rows:Rc<dyn Fn()->usize>,render_request:Rc<dyn Fn()>,done:Rc<dyn Fn()>)->Self{
        Self{markdown:Markdown::new(markdown,1,0,markdown_theme(theme),None,MarkdownOptions::default()),rows,render_request,done,offset:0,line_count:0,focused:false}
    }
    fn viewport_height(&self)->usize{(self.rows)().saturating_sub(HELP_OVERLAY_MARGIN*2).max(1)}
    fn max_offset(&self)->usize{self.line_count.saturating_sub(self.viewport_height())}
}
impl Focusable for HelpPanel{fn focused(&self)->bool{self.focused}fn set_focused(&mut self,value:bool){self.focused=value;}}
impl Component for HelpPanel{
    fn render(&mut self,width:usize)->Vec<String>{let lines=self.markdown.render(width);self.line_count=lines.len();self.offset=self.offset.min(self.max_offset());lines.into_iter().skip(self.offset).take(self.viewport_height()).collect()}
    fn handle_input(&mut self,data:&str){
        let keys=get_keybindings();if keys.matches(data,"tui.select.cancel"){(self.done)();return;}
        let before=self.offset;
        if keys.matches(data,"tui.select.up"){self.offset=self.offset.saturating_sub(1);}
        else if keys.matches(data,"tui.select.down"){self.offset=(self.offset+1).min(self.max_offset());}
        else if keys.matches(data,"tui.select.pageUp"){self.offset=self.offset.saturating_sub(self.viewport_height());}
        else if keys.matches(data,"tui.select.pageDown"){self.offset=(self.offset+self.viewport_height()).min(self.max_offset());}
        if before!=self.offset{(self.render_request)();}
    }
    fn has_input_handler(&self)->bool{true}
    fn invalidate(&mut self){self.markdown.invalidate();}
    fn focusable_get(&self)->Option<bool>{Some(self.focused)}
    fn focusable_set(&mut self,value:bool){self.set_focused(value);}
}
