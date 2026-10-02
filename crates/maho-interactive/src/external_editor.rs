use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalEditorResult { Complete { content: String }, Failed, LaunchFailed }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditFileResult { Complete, Exited { code: i32 }, LaunchFailed }

fn launch_editor(command: &str, path: &Path) -> io::Result<EditFileResult> {
    let mut parts = command.split(' ');
    let editor = parts.next().unwrap_or_default();
    let mut child = Command::new(editor);
    child.args(parts).arg(path).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    let mut child = match child.spawn() {
        Ok(child) => child,
        Err(_) => return Ok(EditFileResult::LaunchFailed),
    };
    let status = child.wait()?;
    Ok(if status.success() { EditFileResult::Complete } else { EditFileResult::Exited { code: status.code().unwrap_or(-1) } })
}

pub async fn edit_file_in_external_editor(command: &str, path: &Path) -> io::Result<EditFileResult> {
    let command = command.to_owned();
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || launch_editor(&command, &path)).await.map_err(io::Error::other)?
}

pub async fn edit_in_external_editor(command: &str, content: &str) -> io::Result<ExternalEditorResult> {
    let directory = tempfile::Builder::new().prefix("pi-editor-").tempdir()?;
    let path = directory.path().join("prompt.md");
    std::fs::write(&path, content)?;
    writeln!(io::stdout(), "Launching external editor: {command}\nPi will resume when the editor exits.")?;
    match edit_file_in_external_editor(command, &path).await? {
        EditFileResult::LaunchFailed => Ok(ExternalEditorResult::LaunchFailed),
        EditFileResult::Exited { .. } => Ok(ExternalEditorResult::Failed),
        EditFileResult::Complete => {
            let content = std::fs::read_to_string(path)?;
            let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
            Ok(ExternalEditorResult::Complete { content: content.strip_suffix('\n').unwrap_or(content).to_owned() })
        }
    }
}
