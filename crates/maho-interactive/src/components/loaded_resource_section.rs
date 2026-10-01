use maho_tui::{components::text::Text, tui::Component};

pub struct LoadedResourceSection {
    body: Text,
    collapsed_text: String,
    expanded_text: String,
    visible: bool,
}

impl LoadedResourceSection {
    pub fn new(collapsed_text: String, expanded_text: String, expanded: bool) -> Self {
        let mut section = Self {
            body: Text::with_padding("", 0, 0), collapsed_text, expanded_text, visible: false,
        };
        section.set_expanded(expanded);
        section
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        let text = if expanded { &self.expanded_text } else { &self.collapsed_text };
        self.visible = !text.is_empty();
        if self.visible { self.body.set_text(text); }
    }
}

impl Component for LoadedResourceSection {
    fn render(&mut self, width: usize) -> Vec<String> {
        if !self.visible { return Vec::new(); }
        let mut lines = self.body.render(width);
        lines.push(String::new());
        lines
    }

    fn invalidate(&mut self) { self.body.invalidate(); }
}
