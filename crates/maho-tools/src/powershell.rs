use crate::{
    bash::*,
    definition::{ToolDefinition, ToolError, ToolFuture},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
pub type PowerShellToolOptions = BashToolOptions;
const UTF8_OUTPUT_PREFIX: &str =
    "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}\n";
pub const POWERSHELL_ARGS: [&str; 5] = [
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-Command",
];
async fn find_executable_on_path(executable: &str) -> Option<String> {
    let lookup = tokio::process::Command::new("where")
        .arg(executable)
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(std::time::Duration::from_millis(5000), lookup)
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text
        .trim()
        .lines()
        .next()?
        .trim_end_matches('\r')
        .to_owned();
    (!first.is_empty() && Path::new(&first).exists()).then_some(first)
}
/// senpi utils/shell.ts `getPowerShellConfig`: Windows only, preferring PowerShell 7 (`pwsh.exe`).
pub async fn get_powershell_config() -> Result<(String, Vec<String>), ToolError> {
    if !cfg!(windows) {
        return Err(ToolError::Message(
            "The powershell tool is only available on Windows.".into(),
        ));
    }
    let shell = match find_executable_on_path("pwsh.exe").await { Some(path) => Some(path), None => find_executable_on_path("powershell.exe").await }
        .ok_or_else(|| ToolError::Message("No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH.".into()))?;
    Ok((
        shell,
        POWERSHELL_ARGS.into_iter().map(str::to_owned).collect(),
    ))
}
struct LocalPowerShellOperations;
impl BashOperations for LocalPowerShellOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a Path,
        options: BashExecOptions,
    ) -> ToolFuture<'a, BashExit> {
        Box::pin(async move {
            resolve_timeout_ms(options.timeout)?;
            if options.signal.is_aborted() {
                return Err(ToolError::Message("aborted".into()));
            }
            let (shell, args) = get_powershell_config().await?;
            let config = crate::shell::ShellConfig {
                shell,
                args,
                command_transport: None,
                kind: None,
            };
            let operations =
                create_local_shell_operations("PowerShell", Arc::new(move || Ok(config.clone())));
            operations
                .exec(&format!("{UTF8_OUTPUT_PREFIX}{command}"), cwd, options)
                .await
        })
    }
}
pub fn create_local_powershell_operations() -> Arc<dyn BashOperations> {
    Arc::new(LocalPowerShellOperations)
}
pub fn create_powershell_tool_definition(
    cwd: PathBuf,
    mut options: PowerShellToolOptions,
) -> ToolDefinition {
    // The default PowerShell operations are local, not caller-injected: only a caller's own operations pin the tool.
    let server_pinned = options.operations.is_some() || options.spawn_hook.is_some();
    if options.operations.is_none() {
        options.operations = Some(create_local_powershell_operations());
    }
    let mut tool = create_shell_tool_definition(
        cwd,
        ShellToolConfig {
            name: "powershell".into(),
            shell_name: "PowerShell".into(),
            prompt_snippet: "Execute PowerShell commands".into(),
            temp_file_prefix: "pi-powershell".into(),
        },
        options,
    );
    tool.server_pinned = server_pinned.then_some(true);
    tool
}
