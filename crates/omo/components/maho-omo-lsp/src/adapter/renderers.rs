use std::sync::Arc;
use maho_ext_api::{Component,ToolCallRenderer,ToolRenderContext,ToolRenderResultOptions,ToolRenderers,ToolResultRenderer};
use serde_json::Value;
use super::rendering::Text;
use super::renderers_common::RenderResultOptions;
use super::renderers_diagnostics::{render_diagnostics_call,render_diagnostics_result};
use super::renderers_navigation::{render_find_references_call,render_find_references_result,render_goto_definition_call,render_goto_definition_result};
use super::renderers_rename::{render_prepare_rename_call,render_prepare_rename_result,render_rename_call,render_rename_result};
use super::renderers_symbols::{render_symbols_call,render_symbols_result};
pub type LspToolRenderers=ToolRenderers<(),Value>;
fn component(text:Text)->Box<dyn Component> {Box::new(maho_tui::components::text::Text::with_padding(text.text,0,0))}
fn options(options:ToolRenderResultOptions)->RenderResultOptions {RenderResultOptions {expanded:options.expanded,is_partial:options.is_partial}}
pub fn tool_renderers(name:&str)->Option<LspToolRenderers> {
    let call:ToolCallRenderer<(),Value>=match name {
        "lsp_diagnostics"=>Arc::new(|a:&Value,t:&maho_ext_api::Theme,_:&mut ToolRenderContext<(),Value>|component(render_diagnostics_call(a,t))),
        "lsp_goto_definition"=>Arc::new(|a:&Value,t:&maho_ext_api::Theme,_:&mut ToolRenderContext<(),Value>|component(render_goto_definition_call(a,t))),
        "lsp_find_references"=>Arc::new(|a:&Value,t:&maho_ext_api::Theme,_:&mut ToolRenderContext<(),Value>|component(render_find_references_call(a,t))),
        "lsp_symbols"=>Arc::new(|a:&Value,t:&maho_ext_api::Theme,_:&mut ToolRenderContext<(),Value>|component(render_symbols_call(a,t))),
        "lsp_prepare_rename"=>Arc::new(|a:&Value,t:&maho_ext_api::Theme,_:&mut ToolRenderContext<(),Value>|component(render_prepare_rename_call(a,t))),
        "lsp_rename"=>Arc::new(|a:&Value,t:&maho_ext_api::Theme,_:&mut ToolRenderContext<(),Value>|component(render_rename_call(a,t))),
        _=>return None,
    };
    let result:ToolResultRenderer<(),Value>=match name {
        "lsp_diagnostics"=>Arc::new(|r:&maho_ext_api::AgentToolResult,o:ToolRenderResultOptions,t:&maho_ext_api::Theme,_:&mut ToolRenderContext<(),Value>|component(render_diagnostics_result(&serde_json::json!({"content":r.content,"details":r.details}),options(o),t))),
        "lsp_goto_definition"=>Arc::new(|r:&maho_ext_api::AgentToolResult,o:ToolRenderResultOptions,t:&maho_ext_api::Theme,_:&mut ToolRenderContext<(),Value>|component(render_goto_definition_result(&serde_json::json!({"content":r.content,"details":r.details}),options(o),t))),
        "lsp_find_references"=>Arc::new(|r:&maho_ext_api::AgentToolResult,o:ToolRenderResultOptions,t:&maho_ext_api::Theme,_:&mut ToolRenderContext<(),Value>|component(render_find_references_result(&serde_json::json!({"content":r.content,"details":r.details}),options(o),t))),
        "lsp_symbols"=>Arc::new(|r:&maho_ext_api::AgentToolResult,o:ToolRenderResultOptions,t:&maho_ext_api::Theme,_:&mut ToolRenderContext<(),Value>|component(render_symbols_result(&serde_json::json!({"content":r.content,"details":r.details}),options(o),t))),
        "lsp_prepare_rename"=>Arc::new(|r:&maho_ext_api::AgentToolResult,o:ToolRenderResultOptions,t:&maho_ext_api::Theme,_:&mut ToolRenderContext<(),Value>|component(render_prepare_rename_result(&serde_json::json!({"content":r.content,"details":r.details}),options(o),t))),
        "lsp_rename"=>Arc::new(|r:&maho_ext_api::AgentToolResult,o:ToolRenderResultOptions,t:&maho_ext_api::Theme,_:&mut ToolRenderContext<(),Value>|component(render_rename_result(&serde_json::json!({"content":r.content,"details":r.details}),options(o),t))),
        _=>return None,
    };
    Some(ToolRenderers {render_call:Some(call),render_result:Some(result)})
}
