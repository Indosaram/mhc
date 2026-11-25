//! Port of `packages/coding-agent/src/extensions/html-render/index.ts` (senpi#5045).

pub mod bootstrap;
pub mod hand_page_budget;
pub mod handed_pages;
pub mod image_references;
pub mod images;
pub mod tool;

use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{Extension, ExtensionApi};

use crate::host_tools::{HostTool, attach_host_tools};
use handed_pages::PendingHandedPages;
use tool::{create_show_html_page_handler, hand_routed_html_page, show_html_page_tool_definition};

#[derive(Default)]
pub struct HtmlRenderExtension {
    pending: Arc<Mutex<PendingHandedPages>>,
}

impl HtmlRenderExtension {
    pub fn reset_pending(&self) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

impl Extension for HtmlRenderExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let pending = self.pending.clone();
        let definition = show_html_page_tool_definition();
        let handler = create_show_html_page_handler(pending.clone());
        let settle_pending = pending.clone();
        let settle: crate::host_tools::SettleRouted =
            Arc::new(move |result, tool_call_id, params, ctx| {
                let pending = settle_pending.clone();
                Box::pin(async move {
                    Ok(hand_routed_html_page(
                        tool_call_id,
                        &params,
                        ctx,
                        &pending,
                        result,
                    ))
                })
            });
        attach_host_tools(
            api,
            vec![HostTool {
                tool: definition,
                execute: handler,
                settle_routed: Some(settle),
            }],
        );
    }
}
