use std::{collections::HashMap, panic::{catch_unwind, AssertUnwindSafe}, path::PathBuf, sync::{Arc, OnceLock}};

use maho_agent::types::AgentToolResult;
use maho_ext_api::{ErasedToolRenderers, JsonValue, Theme};
use regex::Regex;

use super::ansi_to_html::ansi_lines_to_html;

/// Looks up the registered renderers for a tool name.
pub type ToolRendererLookup = Arc<dyn Fn(&str) -> Option<Arc<dyn ErasedToolRenderers>> + Send + Sync>;

pub struct ToolHtmlRendererDeps {
    pub get_tool_renderer: ToolRendererLookup,
    pub theme: Theme,
    pub cwd: PathBuf,
    pub width: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolHtmlResult {
    pub collapsed: Option<String>,
    pub expanded: String,
}

pub struct ToolHtmlRenderer {
    get_tool_renderer: ToolRendererLookup,
    theme: Theme,
    #[allow(dead_code)]
    cwd: PathBuf,
    width: usize,
    rendered_args: HashMap<String, JsonValue>,
    rendered_states: HashMap<String, ToolRenderState>,
}

#[derive(Default)]
struct ToolRenderState {
    call_lines: Option<Vec<String>>,
    collapsed_lines: Option<Vec<String>>,
    result_lines: Option<Vec<String>>,
}

impl ToolHtmlRenderer {
    pub fn new(deps: ToolHtmlRendererDeps) -> Self {
        Self {
            get_tool_renderer: deps.get_tool_renderer,
            theme: deps.theme,
            cwd: deps.cwd,
            width: deps.width.unwrap_or(100),
            rendered_args: HashMap::new(),
            rendered_states: HashMap::new(),
        }
    }

    pub fn render_call(&mut self, tool_call_id: &str, tool_name: &str, args: JsonValue) -> Option<String> {
        let renderer = (self.get_tool_renderer)(tool_name)?;
        self.rendered_args.insert(tool_call_id.to_owned(), args.clone());
        let state = self.rendered_states.entry(tool_call_id.to_owned()).or_default();
        catch_unwind(AssertUnwindSafe(|| {
            let lines = renderer.render_call(&args, &self.theme, self.width)?;
            state.call_lines = Some(lines.clone());
            Some(ansi_lines_to_html(&lines))
        })).ok().flatten()
    }

    pub fn render_result(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        result: &AgentToolResult,
        _is_error: bool,
    ) -> Option<ToolHtmlResult> {
        let renderer = (self.get_tool_renderer)(tool_name)?;
        let state = self.rendered_states.entry(tool_call_id.to_owned()).or_default();
        catch_unwind(AssertUnwindSafe(|| {
            let args = self.rendered_args.get(tool_call_id).unwrap_or(&JsonValue::Null);
            let collapsed_lines = renderer.render_result(args, result, &self.theme, self.width)?;
            let collapsed_lines = trim_rendered_result_lines(collapsed_lines);
            let expanded_lines = renderer.render_result(args, result, &self.theme, self.width)?;
            let expanded_lines = trim_rendered_result_lines(expanded_lines);
            state.collapsed_lines = Some(collapsed_lines.clone());
            state.result_lines = Some(expanded_lines.clone());
            let collapsed = ansi_lines_to_html(&collapsed_lines);
            let expanded = ansi_lines_to_html(&expanded_lines);
            Some(ToolHtmlResult { collapsed: (collapsed != expanded).then_some(collapsed), expanded })
        })).ok().flatten()
    }

    pub fn rendered_html(&self, tool_call_id: &str) -> Option<serde_json::Value> {
        let state = self.rendered_states.get(tool_call_id)?;
        let mut rendered = serde_json::Map::new();
        if let Some(lines) = &state.call_lines { rendered.insert("callHtml".into(), ansi_lines_to_html(lines).into()); }
        if let Some(lines) = &state.collapsed_lines { rendered.insert("resultHtmlCollapsed".into(), ansi_lines_to_html(lines).into()); }
        if let Some(lines) = &state.result_lines { rendered.insert("resultHtmlExpanded".into(), ansi_lines_to_html(lines).into()); }
        (!rendered.is_empty()).then_some(serde_json::Value::Object(rendered))
    }
}

fn is_blank_rendered_line(line: &str) -> bool {
    static ANSI_ESCAPE_REGEX: OnceLock<Regex> = OnceLock::new();
    let regex = ANSI_ESCAPE_REGEX.get_or_init(|| Regex::new(r"\x1b\[[\d;]*m").expect("valid ANSI SGR regex"));
    regex.replace_all(line, "").trim().is_empty()
}

fn trim_rendered_result_lines(lines: Vec<String>) -> Vec<String> {
    let mut start = 0;
    let mut end = lines.len();
    while start < end && is_blank_rendered_line(&lines[start]) { start += 1; }
    while end > start && is_blank_rendered_line(&lines[end - 1]) { end -= 1; }
    lines[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_lines_ignore_ansi_sgr_codes() {
        assert!(is_blank_rendered_line("\x1b[31m  \x1b[0m"));
        assert!(!is_blank_rendered_line("\x1b[31mx\x1b[0m"));
    }

    #[test]
    fn result_lines_trim_blank_edges_but_preserve_inner_lines() {
        assert_eq!(
            trim_rendered_result_lines(vec!["  ".into(), "first".into(), "".into(), "last".into(), "\x1b[0m".into()]),
            vec!["first", "", "last"]
        );
    }

    #[test]
    fn result_lines_all_blank_become_empty() {
        assert!(trim_rendered_result_lines(vec!["".into(), "\x1b[1m  \x1b[0m".into()]).is_empty());
    }

}
