use maho_core::{
    config::get_debug_log_path,
    output_guard::{restore_stderr, take_over_stderr},
    sensitive_output::redact_sensitive_output,
};
use std::{
    io::{self, Write},
    path::Path,
    sync::Arc,
};

pub fn append_hidden_interactive_stderr(
    path: &Path,
    text: &str,
    timestamp: &str,
) -> io::Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let redacted = redact_sensitive_output(text);
    write!(
        file,
        "[{timestamp}] hidden stderr while TUI active\n{redacted}{}",
        if redacted.ends_with('\n') { "" } else { "\n" }
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn take_over_interactive_stderr() {
    take_over_stderr(
        Some(Arc::new(|text| {
            let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            if let Err(error) =
                append_hidden_interactive_stderr(Path::new(&get_debug_log_path()), text, &timestamp)
            {
                // The existing output guard catches a failed diagnostic sink and emits its redacted fallback.
                std::panic::resume_unwind(Box::new(error));
            }
        })),
        Some(Arc::new(redact_sensitive_output)),
    );
}

pub fn restore_interactive_stderr() {
    restore_stderr();
}

#[cfg(test)]
mod direct_stderr_tests {
    /// senpi routes every interactive diagnostic through `process.stderr`/`console`, which the
    /// guard intercepts. In Rust a bare `eprintln!` writes fd 2 behind the guard, so interactive
    /// sources must go through `maho_write_stderr`.
    #[test]
    fn interactive_sources_never_write_stderr_directly() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut pending = vec![root];
        let mut offenders = Vec::new();
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("read source");
                let production = source.split("#[cfg(test)]").next().unwrap_or_default();
                for (index, line) in production.lines().enumerate() {
                    let code = line.trim_start();
                    if code.starts_with("//") {
                        continue;
                    }
                    if ["eprintln!(", "eprint!(", "dbg!(", "io::stderr()"]
                        .iter()
                        .any(|needle| code.contains(needle))
                    {
                        offenders.push(format!("{}:{}", path.display(), index + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "direct stderr writes bypass the interactive guard: {offenders:?}"
        );
    }
}
