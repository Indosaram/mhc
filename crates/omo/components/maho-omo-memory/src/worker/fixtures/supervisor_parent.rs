use std::{io::Write, path::Path, process::Stdio};
use tokio::io::AsyncReadExt;
pub async fn run(executable: &Path, prefix: &[String], run_dir: &Path) -> Result<(), String> {
    let mut command = tokio::process::Command::new(executable);
    command.args(prefix).arg(run_dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let pid = child.id();
    tokio::spawn(async move {
        if let Err(error) = child.wait().await { eprintln!("{error}"); }
    });
    let mut output = std::io::stdout().lock();
    writeln!(output, "{}", serde_json::json!({"supervisorPid":pid})).and_then(|()| output.flush()).map_err(|error| error.to_string())?;
    drop(output);
    let mut input = [0_u8; 1024];
    while tokio::io::stdin().read(&mut input).await.map_err(|error| error.to_string())? > 0 {}
    Ok(())
}
