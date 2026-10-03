use std::{collections::BTreeMap, rc::Rc};
use maho_ext_api::{ExtensionTuiHost, ReadonlyFooterDataProvider, UiUnsubscribe};

pub struct InteractiveUiHost(pub Rc<dyn maho_tui::components::editor::EditorTuiHost>, pub Rc<std::cell::Cell<(u16,u16)>>);
impl ExtensionTuiHost for InteractiveUiHost {
    fn request_render(&self) { self.0.request_render(); }
    fn dimensions(&self) -> (u16,u16) { self.1.get() }
}

pub struct InteractiveFooterData {
    pub provider: std::sync::Arc<maho_core::footer_data_provider::FooterDataProvider>,
    pub ui: std::sync::Arc<crate::interactive_extension_ui::InteractiveExtensionUi>,
}
impl ReadonlyFooterDataProvider for InteractiveFooterData {
    fn get_git_branch(&self) -> Option<String> { self.provider.git_branch() }
    fn get_extension_statuses(&self) -> BTreeMap<String,String> { self.ui.statuses.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() }
    fn get_available_provider_count(&self) -> usize { self.provider.available_provider_count() }
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

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::ExtensionUi;
    #[test]
    fn footer_factory_data_reads_live_statuses_and_provider_count() {
        let (ui,_requests)=crate::interactive_extension_ui::InteractiveExtensionUi::channel(Default::default());
        let provider=std::sync::Arc::new(maho_core::footer_data_provider::FooterDataProvider::new("/tmp"));
        let data=InteractiveFooterData {provider:provider.clone(),ui:ui.clone()};
        assert!(data.get_extension_statuses().is_empty());
        ui.set_status("worker",Some("running"));
        assert_eq!(data.get_extension_statuses().get("worker").map(String::as_str),Some("running"));
        provider.set_available_provider_count(2);
        assert_eq!(data.get_available_provider_count(),2);
        ui.set_status("worker",None);
        assert!(data.get_extension_statuses().is_empty());
    }
}
