//! Port of `core/tools/renderers/index.ts`: the built-in tool renderers, keyed by tool name.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use maho_tools::definition::{RenderShell, ToolResult};
use maho_tui::tui::Component;
use serde_json::Value;

use crate::theme::Theme;

pub mod bash;
pub mod edit;
pub mod find;
pub mod grep;
pub mod ls;
pub mod read;
pub mod write;

#[derive(Clone, Copy, Debug, Default)]
pub struct ToolRenderResultOptions {
    pub expanded: bool,
    pub is_partial: bool,
}

pub struct ToolRenderContext<'a> {
    pub args: &'a Value,
    pub tool_call_id: &'a str,
    pub cwd: &'a str,
    pub execution_started: bool,
    pub args_complete: bool,
    pub is_partial: bool,
    pub expanded: bool,
    pub show_images: bool,
    pub is_error: bool,
    pub has_result: bool,
    pub spinner_frame: Option<i64>,
    pub now_ms: f64,
    pub invalidate: Rc<dyn Fn()>,
}

pub type RenderedComponent = Rc<RefCell<dyn Component>>;

pub trait ToolRenderers {
    fn render_shell(&self) -> RenderShell {
        RenderShell::Default
    }
    /// senpi keeps a card out of an exploration group when its definition replaced the built-in
    /// renderers, and compares the two functions by reference to decide that. Rust has no function
    /// identity, so a renderer reports whether both of its halves are the built-in ones; a
    /// definition that supplies its own renderers leaves this `false`.
    fn is_built_in(&self) -> bool {
        false
    }
    fn render_call(&mut self, theme: &Theme, context: &ToolRenderContext<'_>) -> Option<RenderedComponent>;
    fn render_result(
        &mut self,
        result: &ToolResult,
        options: ToolRenderResultOptions,
        theme: &Theme,
        context: &ToolRenderContext<'_>,
    ) -> Option<RenderedComponent>;
}

pub fn create_all_tool_renderers() -> HashMap<&'static str, Rc<RefCell<dyn ToolRenderers>>> {
    HashMap::from([
        ("read", Rc::new(RefCell::new(read::ReadRenderers::default())) as Rc<RefCell<dyn ToolRenderers>>),
        ("bash", Rc::new(RefCell::new(bash::ShellRenderers::new("$"))) as Rc<RefCell<dyn ToolRenderers>>),
        ("powershell", Rc::new(RefCell::new(bash::ShellRenderers::new("PS>"))) as Rc<RefCell<dyn ToolRenderers>>),
        ("edit", Rc::new(RefCell::new(edit::EditRenderers::default())) as Rc<RefCell<dyn ToolRenderers>>),
        ("write", Rc::new(RefCell::new(write::WriteRenderers::default())) as Rc<RefCell<dyn ToolRenderers>>),
        ("grep", Rc::new(RefCell::new(grep::GrepRenderers)) as Rc<RefCell<dyn ToolRenderers>>),
        ("find", Rc::new(RefCell::new(find::FindRenderers)) as Rc<RefCell<dyn ToolRenderers>>),
        ("ls", Rc::new(RefCell::new(ls::LsRenderers)) as Rc<RefCell<dyn ToolRenderers>>),
    ])
}

/// senpi's `withBuiltInRenderers`.
///
/// senpi's version fills in each half a partial definition left out; a Rust [`ToolRenderers`] is
/// always complete, so a definition that supplies its own renderers simply wins and one that has
/// none falls back to the built-in pair.
pub fn with_built_in_renderers(
    tool_name: &str,
    definition: Option<Rc<RefCell<dyn ToolRenderers>>>,
) -> Option<Rc<RefCell<dyn ToolRenderers>>> {
    match definition {
        Some(definition) => Some(definition),
        None => create_all_tool_renderers().get(tool_name).cloned(),
    }
}
