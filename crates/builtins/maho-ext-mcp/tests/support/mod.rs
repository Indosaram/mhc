pub mod clock;

use std::{env, path::PathBuf};

#[allow(dead_code)]
pub fn node() -> String {
    let binary = if cfg!(windows) { "node.exe" } else { "node" };
    env::split_paths(&env::var_os("PATH").expect("PATH must be set to locate node"))
        .map(|dir| dir.join(binary))
        .find(|path| path.is_file())
        .or_else(|| {
            if cfg!(windows) {
                env::split_paths(&env::var_os("PATH").unwrap_or_default())
                    .map(|dir| dir.join("node"))
                    .find(|path| path.is_file())
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("could not find executable node on PATH"))
        .to_string_lossy()
        .into_owned()
}

#[allow(dead_code)]
pub fn senpi(rel: &str) -> String {
    let root = env::var_os("SENPI_SRC")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = env::var_os("HOME")
                .or_else(|| env::var_os("USERPROFILE"))
                .expect("HOME or USERPROFILE must be set");
            PathBuf::from(home).join("code/senpi")
        });
    root.join(rel).to_string_lossy().into_owned()
}

/// Deterministic exported theme for renderer assertions: distinct truecolor prefixes per role.
#[allow(dead_code)]
pub fn render_theme() -> maho_ext_api::Theme {
    maho_ext_api::Theme {
        colors: [
            ("toolTitle", "\x1b[38;2;1;2;3m"),
            ("toolOutput", "\x1b[38;2;4;5;6m"),
        ]
        .map(|(key, value)| (key.into(), value.into()))
        .into(),
        ..Default::default()
    }
}

#[allow(dead_code)]
fn render_session(
    renderers: maho_ext_mcp::tool_registrar::McpToolRenderers,
    args: serde_json::Value,
    is_partial: bool,
) -> maho_ext_api::ToolRendererSession<(), serde_json::Value> {
    maho_ext_api::ToolRendererSession {
        renderers: std::sync::Arc::new(renderers),
        context: maho_ext_api::ToolRenderContext {
            args,
            tool_call_id: "render".into(),
            invalidate: std::rc::Rc::new(|| {}),
            last_component: None,
            state: (),
            cwd: Default::default(),
            execution_started: false,
            args_complete: true,
            is_partial,
            expanded: false,
            show_images: false,
            image_protocol: None,
            is_error: false,
            has_result: None,
            spinner_frame: None,
        },
    }
}

/// `renderCall` lines at `width`; `None` when the renderer set has no call half.
#[allow(dead_code)]
pub fn render_call_lines(
    renderers: maho_ext_mcp::tool_registrar::McpToolRenderers,
    args: serde_json::Value,
    width: usize,
) -> Option<Vec<String>> {
    render_session(renderers, args, false).render_call(&render_theme(), width)
}

/// `renderResult` lines at `width`; `None` when the renderer set has no result half.
#[allow(dead_code)]
pub fn render_result_lines(
    renderers: maho_ext_mcp::tool_registrar::McpToolRenderers,
    result: &maho_ext_api::AgentToolResult,
    is_partial: bool,
    width: usize,
) -> Option<Vec<String>> {
    render_session(renderers, serde_json::Value::Null, is_partial).render_result(
        result,
        &render_theme(),
        width,
    )
}

/// One `Text(…, 0, 0)` line: the styled text padded with spaces to `width` visible columns.
#[allow(dead_code)]
pub fn padded(styled: &str, visible: &str, width: usize) -> String {
    format!(
        "{styled}{}",
        " ".repeat(width - visible.encode_utf16().count())
    )
}
