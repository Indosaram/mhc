//! Port of senpi core/extensions/builtin/html-render (index.ts, tool.ts, bootstrap.ts, images.ts).
pub mod bootstrap;
pub mod images;
pub mod tool;

use maho_ext_api::{Extension, ExtensionApi};

/// `htmlRenderExtension`: registers `show_html_page`.
#[derive(Clone, Copy, Default)]
pub struct HtmlRender;

impl Extension for HtmlRender {
    fn register(&self, api: &mut ExtensionApi) {
        tool::register_show_html_page_tool(api);
    }
}
