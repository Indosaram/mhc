use std::{collections::BTreeMap, rc::Rc};
use maho_ext_api::{ExtensionTuiHost, ReadonlyFooterDataProvider, UiUnsubscribe};

pub struct InteractiveUiHost(pub Rc<dyn maho_tui::components::editor::EditorTuiHost>, pub Rc<std::cell::Cell<(u16,u16)>>);
impl ExtensionTuiHost for InteractiveUiHost {
    fn request_render(&self) { self.0.request_render(); }
    fn dimensions(&self) -> (u16,u16) { self.1.get() }
}

pub struct InteractiveFooterData {
    pub provider: maho_core::footer_data_provider::FooterDataProvider,
    pub statuses: BTreeMap<String,String>,
    pub providers: usize,
}
impl ReadonlyFooterDataProvider for InteractiveFooterData {
    fn get_git_branch(&self) -> Option<String> { self.provider.git_branch() }
    fn get_extension_statuses(&self) -> BTreeMap<String,String> { self.statuses.clone() }
    fn get_available_provider_count(&self) -> usize { self.providers }
    fn on_branch_change(&self, callback:std::sync::Arc<dyn Fn()+Send+Sync>) -> UiUnsubscribe {
        let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let subscribed = active.clone();
        self.provider.on_branch_change(std::sync::Arc::new(move || { if subscribed.load(std::sync::atomic::Ordering::Relaxed) { callback(); } }));
        Box::new(move || active.store(false,std::sync::atomic::Ordering::Relaxed))
    }
}

pub struct BoxedComponent(pub Box<dyn maho_tui::tui::Component>);
impl maho_tui::tui::Component for BoxedComponent {
    fn render(&mut self, width:usize)->Vec<String> { self.0.render(width) }
    fn handle_input(&mut self, input:&str) { self.0.handle_input(input); }
    fn has_input_handler(&self)->bool { self.0.has_input_handler() }
    fn invalidate(&mut self) { self.0.invalidate(); }
    fn dispose(&mut self) { self.0.dispose(); }
    fn focusable_get(&self)->Option<bool> { self.0.focusable_get() }
    fn focusable_set(&mut self, focused:bool) { self.0.focusable_set(focused); }
    fn handle_mouse(&mut self, event:&maho_tui::tui::TuiMouseEvent)->Option<maho_tui::tui::TuiMouseEventResult> { self.0.handle_mouse(event) }
}

pub struct OverlayComponent {
    pub component: Rc<std::cell::RefCell<BoxedComponent>>,
    pub renderer: std::rc::Weak<std::cell::RefCell<crate::tui_renderer::InteractiveTui>>,
}
impl maho_tui::tui::Component for OverlayComponent {
    fn render(&mut self, _:usize)->Vec<String> { Vec::new() }
    fn dispose(&mut self) {
        if let Some(renderer) = self.renderer.upgrade() { renderer.borrow_mut().base_mut().hide_overlay(); }
        self.component.borrow_mut().dispose();
    }
}
