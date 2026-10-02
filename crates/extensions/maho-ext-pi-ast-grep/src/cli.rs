#[derive(Clone, Default)]
pub struct RunSgOptions {
    pub pattern: String, pub lang: String, pub paths: Vec<String>, pub globs: Vec<String>,
    pub rewrite: Option<String>, pub context: Option<f64>, pub update_all: bool,
}

pub fn build_sg_args(options: &RunSgOptions, include_update_all: bool) -> Vec<String> {
    let write_pass = options.update_all && !include_update_all;
    let mut args = vec!["run".into(), "-p".into(), options.pattern.clone(), "--lang".into(), options.lang.clone()];
    if !write_pass { args.push("--json=compact".into()); }
    if let Some(rewrite) = options.rewrite.as_ref().filter(|value| !value.is_empty()) {
        args.extend(["-r".into(), rewrite.clone()]);
        if include_update_all { args.push("--update-all".into()); }
    }
    if let Some(context) = options.context.filter(|value| *value > 0.0) { args.extend(["-C".into(), context.to_string()]); }
    for glob in &options.globs { args.extend(["--globs".into(), glob.clone()]); }
    if options.paths.is_empty() { args.push(".".into()); } else { args.extend(options.paths.iter().cloned()); }
    args
}

pub async fn run_sg(options: &RunSgOptions, binary: &std::path::Path) -> crate::types::SgResult {
    match run_sg_once(options, binary).await {
        Ok(result) => result,
        Err(error) => crate::types::SgResult { error: Some(format!("Failed to spawn ast-grep: {error}")), ..Default::default() },
    }
}

pub async fn run_sg_resolved(options: &RunSgOptions, resolver: &crate::binary_path::BinaryResolver) -> crate::types::SgResult {
    let Some(binary) = resolver.resolve().await else { return crate::types::SgResult { error: Some(INSTALL_HINT.into()), ..Default::default() }; };
    match run_sg_once(options, &binary).await {
        Ok(result) => result,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound || error.to_string().contains("not found") => {
            let downloaded = crate::downloader::ensure_ast_grep_binary(&resolver.cache, &resolver.platform_key, &resolver.version, resolver.offline).await;
            if downloaded.is_some() && let Some(binary) = resolver.resolve().await {
                    match run_sg_once(options, &binary).await {
                        Ok(result) => return result,
                        Err(error) if error.kind() != std::io::ErrorKind::NotFound && !error.to_string().contains("not found") => return crate::types::SgResult { error: Some(format!("Failed to spawn ast-grep: {error}")), ..Default::default() },
                        Err(_) => {}
                    }
            }
            crate::types::SgResult { error: Some(AUTO_DOWNLOAD_FAILED_HINT.into()), ..Default::default() }
        }
        Err(error) => crate::types::SgResult { error: Some(format!("Failed to spawn ast-grep: {error}")), ..Default::default() },
    }
}

pub const INSTALL_HINT: &str = "ast-grep (sg) binary not found.\n\nInstall options:\n  npm install -g @ast-grep/cli\n  cargo install ast-grep --locked\n  brew install ast-grep";
pub const AUTO_DOWNLOAD_FAILED_HINT: &str = "ast-grep CLI binary not found.\n\nAuto-download failed. Manual install options:\n  npm install -g @ast-grep/cli\n  cargo install ast-grep --locked\n  brew install ast-grep";

async fn run_sg_once(options: &RunSgOptions, binary: &std::path::Path) -> Result<crate::types::SgResult, std::io::Error> {
    use crate::{json_output::create_sg_result_from_stdout, types::{SgResult, TruncationReason}};
    async fn spawn(binary: &std::path::Path, args: &[String]) -> Result<crate::process_timeout::ProcessOutput, crate::errors::ProcessError> {
        let mut command = tokio::process::Command::new(binary);
        command.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        let child = command.spawn().map_err(crate::errors::ProcessError::Spawn)?;
        crate::process_timeout::collect_process_output_with_timeout(child, 300_000).await
    }
    let separate = options.update_all && options.rewrite.as_ref().is_some_and(|value| !value.is_empty());
    let mut read_options = options.clone();
    if separate { read_options.update_all = false; }
    let args = build_sg_args(&read_options, !separate);
    let output = match spawn(binary, &args).await {
        Ok(output) => output,
        Err(error @ crate::errors::ProcessError::Timeout(_)) => return Ok(SgResult { truncated: true, truncated_reason: Some(TruncationReason::Timeout), error: Some(error.to_string()), ..Default::default() }),
        Err(crate::errors::ProcessError::Spawn(error)) => return Err(error),
    };
    let stdout = output.stdout;
    let stderr = output.stderr;
    if output.exit_code != 0 && stdout.trim().is_empty() {
        if stderr.contains("No files found") || stderr.trim().is_empty() { return Ok(SgResult::default()); }
        return Ok(SgResult { error: Some(stderr.trim().to_owned()), ..Default::default() });
    }
    let mut result = create_sg_result_from_stdout(&stdout);
    if separate && !result.matches.is_empty() {
        let mut args = build_sg_args(options, false);
        args.push("--update-all".to_owned());
        match spawn(binary, &args).await {
            Ok(output) if output.exit_code != 0 => {
                let stderr = output.stderr;
                let detail = if stderr.trim().is_empty() { format!("ast-grep exited with code {}", output.exit_code) } else { stderr.trim().to_owned() };
                result.error = Some(format!("Replace failed: {detail}"));
            }
            Ok(_) => {}
            Err(error) => result.error = Some(format!("Replace failed: {error}")),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn missing_interpreter_reports_failed_download_not_generic_spawn() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().expect("resolver fixture");
        let binary = fixture.path().join("sg");
        let script = format!("#!{}/absent-interpreter\n{}", fixture.path().display(), "#".repeat(10_001));
        std::fs::write(&binary, script).expect("write absent interpreter binary");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("executable fixture");
        let resolver = crate::binary_path::BinaryResolver::new(fixture.path().join("extension.rs"), fixture.path().join("cache"), Some(fixture.path().as_os_str().to_owned()), "linux-x64".into(), true);
        let result = run_sg_resolved(&RunSgOptions::default(), &resolver).await;
        assert_eq!(result.error.as_deref(), Some(AUTO_DOWNLOAD_FAILED_HINT));
        assert!(!result.truncated);
    }
    fn options() -> RunSgOptions { RunSgOptions { pattern: "console.log($MSG)".into(), lang: "typescript".into(), paths: vec!["src".into()], ..Default::default() } }
    #[test] fn compact_when_search() { assert_eq!(build_sg_args(&options(), false), ["run", "-p", "console.log($MSG)", "--lang", "typescript", "--json=compact", "src"]); }
    #[test] fn context_when_positive() { let value = RunSgOptions { context: Some(3.0), ..options() }; assert_eq!(&build_sg_args(&value, false)[6..], ["-C", "3", "src"]); }
    #[test] fn rewrite_when_dry_pass() { let value = RunSgOptions { rewrite: Some("logger.info($MSG)".into()), ..options() }; assert!(!build_sg_args(&value, false).iter().any(|arg| arg == "--update-all")); }
    #[test] fn update_when_included() { let value = RunSgOptions { rewrite: Some("logger.info($MSG)".into()), ..options() }; assert!(build_sg_args(&value, true).iter().any(|arg| arg == "--update-all")); }
    #[test] fn repeated_when_globs() { let value = RunSgOptions { globs: vec!["**/*.ts".into(), "!**/*.test.ts".into()], ..options() }; assert_eq!(build_sg_args(&value, false).iter().filter(|arg| *arg == "--globs").count(), 2); }
    #[test] fn current_when_default_paths() { let value = RunSgOptions { paths: Vec::new(), ..options() }; assert_eq!(build_sg_args(&value, false).last().map(String::as_str), Some(".")); }
    #[test] fn current_when_empty_options() { assert_eq!(build_sg_args(&RunSgOptions::default(), false).last().map(String::as_str), Some(".")); }
    #[test] fn non_json_when_write_pass() { let value = RunSgOptions { rewrite: Some("x".into()), update_all: true, ..options() }; assert!(!build_sg_args(&value, false).iter().any(|arg| arg == "--json=compact")); }
}
