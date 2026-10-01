//! Port of components/dynamic-border.ts.
use std::rc::Rc;
use maho_tui::tui::Component;
use crate::theme::{Theme, ThemeColor};

pub struct DynamicBorder {
    color: Rc<dyn Fn(&str) -> String>,
}
impl DynamicBorder {
    pub fn new(theme: Theme) -> Self {
        Self::with_color(Rc::new(move |text| theme.fg(ThemeColor::Border, text)))
    }
    pub fn with_color(color: Rc<dyn Fn(&str) -> String>) -> Self {
        Self { color }
    }
}
impl Component for DynamicBorder {
    fn render(&mut self, width: usize) -> Vec<String> {
        vec![(self.color)(&"─".repeat(width.max(1))) ]
    }
}
