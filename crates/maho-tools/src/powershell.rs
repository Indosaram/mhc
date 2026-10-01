use std::{path::PathBuf, sync::Arc};
use crate::{bash::*, definition::ToolDefinition};
pub type PowerShellToolOptions = BashToolOptions;
pub fn create_local_powershell_operations() -> Arc<dyn BashOperations> {
    Arc::new(LocalShellOperations { shell_name: "PowerShell".into(), shell: "pwsh".into(), args: vec!["-NoLogo".into(), "-NoProfile".into(), "-Command".into()], prefix: "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}\n".into() })
}
pub fn create_powershell_tool_definition(cwd: PathBuf, mut options: PowerShellToolOptions) -> ToolDefinition {
    if options.operations.is_none() { options.operations = Some(create_local_powershell_operations()); }
    create_shell_tool_definition(cwd, ShellToolConfig { name: "powershell".into(), shell_name: "PowerShell".into(), prompt_snippet: "Execute PowerShell commands".into(), temp_file_prefix: "pi-powershell".into() }, options)
}
