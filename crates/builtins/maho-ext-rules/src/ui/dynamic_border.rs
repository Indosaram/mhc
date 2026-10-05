use std::sync::Arc;
use maho_tui::tui::Component;

pub struct DynamicBorder { color: Arc<dyn Fn(&str) -> String + Send + Sync> }
impl DynamicBorder {
    pub fn new(color: Arc<dyn Fn(&str) -> String + Send + Sync>) -> Self { Self { color } }
}
impl Component for DynamicBorder {
    fn render(&mut self, width: usize) -> Vec<String> { vec![(self.color)(&"─".repeat(width.max(1)))] }
    fn invalidate(&mut self) {}
}
