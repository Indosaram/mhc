//! Computer tool definition matching upstream `src/tool-definition.ts`.

use std::sync::Arc;
use maho_tools::definition::{
    ToolDefinition, ToolExecutionMode, ToolExposure,
};
use senpi_desktop_prelude::ComputerPreludeAssets;
pub use maho_tools::KernelPrelude;

use crate::params::computer_params_schema;
use crate::permission::computer_permission_parser;

pub const COMPUTER_TOOL_NAME: &str = "computer";

pub const SEARCH_KEYWORDS: &[&str] = &[
    "computer use",
    "computer-use",
    "cua",
    "desktop",
    "gui",
    "screenshot",
    "click",
    "type",
    "keyboard",
    "mouse",
    "window",
    "accessibility",
    "ax",
    "clipboard",
    "automation",
];

pub const COMPUTER_DESCRIPTION: &[&str] = &[
    "Drive the user's real desktop (experimental): windows, screenshots, native mouse and keyboard input, the OS accessibility (AX) tree, and the clipboard. Not a browser.",
    r#"- `{action:"call", chain}` runs one desktop helper, optionally followed by one call on the window/element it returns, e.g. `[{method:"window",args:[{app:"Code"}]},{method:"screenshot"}]`."#,
    r#"- `{action:"run", code, read_only?, timeout?}` runs a JavaScript async function body with `desktop`, `wait`, `assert`, and `tool` in scope; `read_only: true` blocks input."#,
    r#"- `{action:"capabilities"}` reports backend, permissions, `stopPath`, and `focusGuard`; availability is unknown until `computer.capabilities()` returns in this session, so make no availability claim before it and repeat the host and reason on `native-unavailable`. `{action:"close"}` ends the desktop session."#,
    "Pass only the fields of the chosen action; any other field is refused with COMPUTER_INVALID_ARGUMENTS.",
    "In eval cells prefer the `computer` global, which wraps these actions. Pointer x,y are pixels of the latest screenshot of the same target.",
];

pub fn safety_guidelines() -> Vec<String> {
    ComputerPreludeAssets::safety()
        .lines()
        .filter(|l| l.starts_with("- "))
        .map(|l| l[2..].to_string())
        .collect()
}

pub fn kernel_prelude_for_computer() -> KernelPrelude {
    KernelPrelude {
        javascript: ComputerPreludeAssets::javascript().to_string(),
        python: ComputerPreludeAssets::python().to_string(),
        documentation: ComputerPreludeAssets::documentation().to_string(),
        exports: ComputerPreludeAssets::exports().iter().map(|s| s.to_string()).collect(),
        declarations: Some(ComputerPreludeAssets::declarations().to_string()),
        safety: Some(ComputerPreludeAssets::safety().to_string()),
        method_allowlist: Some(
            ComputerPreludeAssets::method_allowlist()
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ),
    }
}

pub fn computer_tool_definition() -> ToolDefinition {
    let execute_stub: maho_tools::definition::ToolExecutor = Arc::new(|_| Box::pin(async { Err(maho_tools::ToolError::Message("computer implementation is not bound".into())) }));
    let mut def = ToolDefinition::new(
        COMPUTER_TOOL_NAME,
        &COMPUTER_DESCRIPTION.join("
"),
        computer_params_schema(),
        execute_stub,
    );
    def.label = "Computer".to_string();
    def.exposure = Some(ToolExposure::Search);
    def.search_text = Some("Operate the real desktop (experimental): screenshots, clicks, typing, key chords, window list, accessibility tree, clipboard; macOS/Linux/Windows".to_string());
    def.search_keywords = Some(SEARCH_KEYWORDS.iter().map(|s| s.to_string()).collect());
    def.search_group = Some("desktop".to_string());
    def.prompt_snippet = Some("Operate the real desktop (experimental): screenshots, native input, accessibility tree, clipboard".to_string());
    def.prompt_guidelines = Some(safety_guidelines());
    def.kernel_prelude = Some(kernel_prelude_for_computer());
    def.permission_parser = Some(Arc::new(|input,cwd|computer_permission_parser(COMPUTER_TOOL_NAME,input,cwd).into_iter().map(Into::into).collect()));
    def.execution_mode = Some(ToolExecutionMode::Sequential);
    def
}
