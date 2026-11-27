//! Port of `packages/coding-agent/src/extensions/index.ts`.

pub mod html_render;

use maho_ext_api::Extension;

pub fn built_in_host_extensions() -> Vec<Box<dyn Extension>> {
    vec![Box::new(html_render::HtmlRenderExtension::default())]
}
