use maho_tui::tui::Component;
pub struct LiveComponentRenderer<C:Component>{component:C,last_lines:Option<Vec<String>>,disposed:bool,render_requested:bool}
impl<C:Component> LiveComponentRenderer<C>{
    pub fn new(component:C)->Self{Self{component,last_lines:None,disposed:false,render_requested:false}}
    pub fn request_render(&mut self){if !self.disposed{self.render_requested=true;}}
    pub fn take_render_request(&mut self)->bool{std::mem::take(&mut self.render_requested)}
    pub fn rerender(&mut self,width:usize)->Option<Vec<String>>{if self.disposed{return None;}let lines=self.component.render(width);if self.last_lines.as_ref()==Some(&lines){return None;}self.last_lines=Some(lines.clone());Some(lines)}
    pub fn render_fault(&mut self){self.last_lines=None;}
    pub fn dispose(&mut self){if self.disposed{return;}self.disposed=true;self.render_requested=false;self.component.dispose();}
}
#[cfg(test)]mod tests{
    use super::*;use std::{cell::Cell,rc::Rc};struct Widget{disposed:Rc<Cell<usize>>}impl Component for Widget{fn render(&mut self,width:usize)->Vec<String>{vec![width.to_string()]}fn dispose(&mut self){self.disposed.set(self.disposed.get()+1);}}
    #[test]fn equal_lines_deduplicate_but_fault_forces_republication(){let mut renderer=LiveComponentRenderer::new(Widget{disposed:Rc::default()});assert_eq!(renderer.rerender(80),Some(vec!["80".into()]));assert!(renderer.rerender(80).is_none());renderer.render_fault();assert!(renderer.rerender(80).is_some());assert_eq!(renderer.rerender(40),Some(vec!["40".into()]));}
    #[test]fn requested_renders_coalesce_and_dispose_cancels_once(){let disposed=Rc::default();let mut renderer=LiveComponentRenderer::new(Widget{disposed:Rc::clone(&disposed)});renderer.request_render();renderer.request_render();assert!(renderer.take_render_request());assert!(!renderer.take_render_request());renderer.request_render();renderer.dispose();renderer.dispose();renderer.request_render();assert!(!renderer.take_render_request());assert!(renderer.rerender(80).is_none());assert_eq!(disposed.get(),1);}
}
