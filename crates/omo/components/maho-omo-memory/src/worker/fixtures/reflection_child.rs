use std::{collections::BTreeMap, io::Write, path::Path};
pub async fn run(mode: &str, env: &BTreeMap<String, String>) -> Result<i32, String> {
    let worktree = env.get("MEMORY_DIR").filter(|path| !path.is_empty()).ok_or("MEMORY_DIR is required")?;
    env.get("TRANSCRIPT_PATH").filter(|path| !path.is_empty()).ok_or("TRANSCRIPT_PATH is required")?;
    if env.get("SENPI_MEMORY_REFLECTION").is_none_or(|value| value != "1") { return Err("reflection sentinel is required".into()); }
    match mode {
        "commit" => {
            std::fs::create_dir_all(Path::new(worktree).join("system")).map_err(|error| error.to_string())?;
            std::fs::write(Path::new(worktree).join("system/reflected.md"), "---\ndescription: A fact learned by the reflection stub\n---\nThe reflection stub merged this fact.\n").map_err(|error| error.to_string())?;
            for args in [vec!["add", "system/reflected.md"], vec!["commit", "-m", "chore(reflection): add stub memory"]] {
                let output = tokio::process::Command::new("git").args(args).current_dir(worktree).output().await.map_err(|error| error.to_string())?;
                if !output.status.success() { return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()); }
            }
            Ok(0)
        }
        "admin" => {
            let mut file = std::fs::OpenOptions::new().append(true).open(Path::new(worktree).join(".git")).map_err(|error| error.to_string())?;
            writeln!(file, "# reflection stub touched git administration").map_err(|error| error.to_string())?;
            Ok(0)
        }
        "timeout" => {
            #[cfg(unix)]
            {
                let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|error| error.to_string())?;
                while term.recv().await.is_some() {}
            }
            std::future::pending().await
        }
        "model-not-found" => {
            eprintln!("Error: Model \"extension-only/primary\" not found. Use --list-models to see available models.");
            Ok(1)
        }
        "auth-missing" => { eprintln!("No API key found for kimi-coding"); Ok(1) }
        _ => Err(format!("unknown reflection child mode: {mode}")),
    }
}
