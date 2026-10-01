//! Port of `components/tool-execution-images.ts`.
//!
//! senpi converts non-PNG images to PNG for the Kitty protocol through a WASM codec that returns
//! `null` when the codec is unavailable; the Rust port has no such codec, so conversion always
//! reports unavailable and the non-PNG Kitty path falls back exactly as senpi's does.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use maho_tools::definition::{ToolContent, ToolResult};
use maho_tui::components::image::{Image, ImageOptions, ImageTheme};
use maho_tui::components::spacer::Spacer;
use maho_tui::components::text::Text;
use maho_tui::terminal_capabilities::ImageProtocol;
use maho_tui::terminal_image::get_capabilities;
use maho_tui::terminal_text::sanitize_terminal_label;
use maho_tui::tui::{Component, Container};

use crate::theme::{Theme, ThemeColor};

#[derive(Clone, Debug)]
pub struct ToolExecutionImageOptions {
    pub show_images: bool,
    pub max_width_cells: u32,
    pub show_renderer_fallback: bool,
}

impl Default for ToolExecutionImageOptions {
    fn default() -> Self {
        Self { show_images: true, max_width_cells: 60, show_renderer_fallback: false }
    }
}

pub fn convert_to_png(base64_data: &str, mime_type: &str) -> Option<(String, String)> {
    if mime_type == "image/png" {
        return Some((base64_data.to_owned(), mime_type.to_owned()));
    }
    None
}

pub struct ToolExecutionImages {
    content: Container,
    result: Option<ToolResult>,
    converted: HashMap<usize, (String, String, String, String)>,
    options: ToolExecutionImageOptions,
    theme: Theme,
}

impl ToolExecutionImages {
    pub fn new(theme: Theme) -> Self {
        Self { content: Container::new(), result: None, converted: HashMap::new(), options: ToolExecutionImageOptions::default(), theme }
    }

    pub fn update_options(&mut self, options: ToolExecutionImageOptions) {
        let normalized = ToolExecutionImageOptions {
            max_width_cells: options.max_width_cells.max(1),
            ..options
        };
        if self.options.show_images == normalized.show_images
            && self.options.max_width_cells == normalized.max_width_cells
            && self.options.show_renderer_fallback == normalized.show_renderer_fallback
        {
            return;
        }
        self.options = normalized;
        self.rebuild();
    }

    pub fn update_result(&mut self, result: &ToolResult) {
        self.result = Some(result.clone());
        self.prune_converted_images();
        self.rebuild();
    }

    fn image_blocks(&self) -> Vec<(String, String)> {
        self.result
            .as_ref()
            .map(|result| {
                result
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ToolContent::Image { data, mime_type } => Some((data.clone(), mime_type.clone())),
                        ToolContent::Text { .. } => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn prune_converted_images(&mut self) {
        let blocks = self.image_blocks();
        self.converted.retain(|index, (source_data, source_mime, _, _)| {
            blocks.get(*index).is_some_and(|(data, mime)| data == source_data && mime == source_mime)
        });
    }

    fn rebuild(&mut self) {
        self.content.clear();
        let capabilities = get_capabilities();
        if !self.options.show_images || capabilities.images.is_none() {
            return;
        }
        let blocks = self.image_blocks();
        for (index, (data, mime_type)) in blocks.iter().enumerate() {
            if data.is_empty() || mime_type.is_empty() {
                continue;
            }
            let (image_data, image_mime_type) = match self.converted.get(&index) {
                Some((source_data, source_mime, converted_data, converted_mime))
                    if source_data == data && source_mime == mime_type =>
                {
                    (converted_data.clone(), converted_mime.clone())
                }
                _ => (data.clone(), mime_type.clone()),
            };
            if capabilities.images == Some(ImageProtocol::Kitty) && image_mime_type != "image/png" {
                if self.options.show_renderer_fallback {
                    let text = self
                        .theme
                        .fg(ThemeColor::ToolOutput, &format!("[image: {}]", sanitize_terminal_label(&image_mime_type)));
                    self.add_image_component(Rc::new(RefCell::new(Text::with_padding(text, 0, 0))));
                }
                continue;
            }
            let theme = self.theme.clone();
            let image = Image::new(
                image_data,
                image_mime_type,
                ImageTheme { fallback_color: Rc::new(move |text| theme.fg(ThemeColor::ToolOutput, text)) },
                ImageOptions { max_width_cells: Some(self.options.max_width_cells), ..Default::default() },
                None,
            );
            self.add_image_component(Rc::new(RefCell::new(image)));
        }
    }

    fn add_image_component(&mut self, component: Rc<RefCell<dyn Component>>) {
        self.content.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        self.content.add_child(component);
    }
}

impl Component for ToolExecutionImages {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.content.render(width)
    }
    fn invalidate(&mut self) {
        self.content.invalidate();
    }
    fn dispose(&mut self) {
        self.content.dispose();
    }
}
