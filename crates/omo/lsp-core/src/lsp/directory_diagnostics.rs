use crate::abort::AbortSignal;
use crate::lsp::client_wrapper::find_workspace_root;
use crate::lsp::client_wrapper::require_found_server;
use crate::lsp::constants::DEFAULT_MAX_DIAGNOSTICS;
use crate::lsp::constants::DEFAULT_MAX_DIRECTORY_FILES;
use crate::lsp::effective_extension::effective_extension;
use crate::lsp::errors::LspError;
use crate::lsp::formatters::filter_diagnostics_by_severity;
use crate::lsp::formatters::format_diagnostic;
use crate::lsp::manager::LspManager;
use crate::lsp::manager::get_lsp_manager;
use crate::lsp::server_resolution::find_server_for_extension;
use crate::lsp::types::Diagnostic;
use crate::lsp::types::ResolvedServer;
use crate::lsp::types::SeverityFilter;
use crate::request_context::context_cwd;
use crate::request_context::resolve_from;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

const SKIP_DIRECTORIES: [&str; 6] = ["node_modules", ".git", "dist", "build", ".next", "out"];
const DIRECTORY_DIAGNOSTICS_MAX_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryDiagnosticsFileFailure {
    pub file: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryDiagnosticsResult {
    pub output: String,
    pub total_diagnostics: usize,
    pub file_failures: Vec<DirectoryDiagnosticsFileFailure>,
}

pub type ListFiles = Arc<dyn Fn(&str, &str, usize) -> Vec<String> + Send + Sync>;

/// TS `DirectoryDiagnosticsOptions`.
#[derive(Clone, Default)]
pub struct DirectoryDiagnosticsOptions {
    pub list_files: Option<ListFiles>,
    pub manager: Option<Arc<LspManager>>,
    pub max_concurrency: Option<usize>,
    pub workspace_root: Option<String>,
    pub server: Option<ResolvedServer>,
    pub signal: Option<AbortSignal>,
}

/// TS `collectFilesWithExtension`: depth-first, skips symlinks and build directories.
pub fn collect_files_with_extension(dir: &str, extension: &str, max_files: usize) -> Vec<String> {
    fn walk(current: &Path, extension: &str, max_files: usize, files: &mut Vec<String>) {
        if files.len() >= max_files {
            return;
        }
        let Ok(entries) = std::fs::read_dir(current) else {
            return;
        };
        let mut names: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        names.sort();
        for name in names {
            if files.len() >= max_files {
                return;
            }
            let full_path = current.join(&name);
            let Ok(metadata) = std::fs::symlink_metadata(&full_path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            let full = full_path.to_string_lossy().into_owned();
            if metadata.is_dir() {
                if !SKIP_DIRECTORIES.contains(&name.to_string_lossy().as_ref()) {
                    walk(&full_path, extension, max_files, files);
                }
            } else if metadata.is_file() && effective_extension(&full) == extension {
                files.push(full);
            }
        }
    }
    let mut files = Vec::new();
    walk(Path::new(dir), extension, max_files, &mut files);
    files
}

/// TS `aggregateDiagnosticsForDirectory`.
pub async fn aggregate_diagnostics_for_directory(
    directory: &str,
    extension: &str,
    severity: Option<SeverityFilter>,
    max_files: Option<usize>,
    options: DirectoryDiagnosticsOptions,
) -> Result<DirectoryDiagnosticsResult, LspError> {
    let max_files = max_files.unwrap_or(DEFAULT_MAX_DIRECTORY_FILES);
    if !extension.starts_with('.') {
        return Err(LspError::InvalidPath(format!(
            "Extension must start with a dot (e.g., \".ts\", not \"{extension}\"). Use \".{extension}\" instead."
        )));
    }
    let base = match &options.workspace_root {
        Some(root) => root.clone(),
        None => context_cwd()?,
    };
    let abs_dir = resolve_from(&base, directory);
    if !Path::new(&abs_dir).exists() {
        return Err(LspError::InvalidPath(format!(
            "Directory does not exist: {abs_dir}"
        )));
    }
    let server = match options.server.clone() {
        Some(server) => server,
        None => require_found_server(find_server_for_extension(extension)?)?,
    };
    let all_files = match &options.list_files {
        Some(list_files) => list_files(&abs_dir, extension, max_files + 1),
        None => collect_files_with_extension(&abs_dir, extension, max_files + 1),
    };
    let was_capped = all_files.len() > max_files;
    let files_to_process: Vec<String> = all_files.into_iter().take(max_files).collect();

    if files_to_process.is_empty() {
        let output = [
            format!("Directory: {abs_dir}"),
            format!("Extension: {extension}"),
            "Files scanned: 0".to_string(),
            format!("No files found with extension \"{extension}\"."),
        ]
        .join("\n");
        return Ok(DirectoryDiagnosticsResult {
            output,
            total_diagnostics: 0,
            file_failures: Vec::new(),
        });
    }

    let root = match &options.workspace_root {
        Some(root) => root.clone(),
        None => find_workspace_root(&abs_dir)?,
    };
    let manager = options.manager.clone().unwrap_or_else(get_lsp_manager);
    let max_concurrency = options
        .max_concurrency
        .unwrap_or(DIRECTORY_DIAGNOSTICS_MAX_CONCURRENCY)
        .max(1);
    let signal = options.signal.as_ref();

    if let Some(signal) = signal {
        signal.throw_if_aborted()?;
    }
    let client = manager.get_client(&root, &server, signal).await?;
    let next_index = Mutex::new(0usize);
    let all_diagnostics: Mutex<Vec<(String, Diagnostic)>> = Mutex::new(Vec::new());
    let file_errors: Mutex<Vec<DirectoryDiagnosticsFileFailure>> = Mutex::new(Vec::new());
    let worker = || async {
        loop {
            if signal.is_some_and(AbortSignal::aborted) {
                return;
            }
            let file = {
                let mut index = next_index.lock().unwrap_or_else(PoisonError::into_inner);
                let file = files_to_process.get(*index).cloned();
                *index += 1;
                file
            };
            let Some(file) = file else {
                return;
            };
            match client.diagnostics(&file, signal).await {
                Ok(result) => {
                    let filtered = filter_diagnostics_by_severity(result.items, severity);
                    all_diagnostics
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .extend(
                            filtered
                                .into_iter()
                                .map(|diagnostic| (file.clone(), diagnostic)),
                        );
                }
                Err(error) => file_errors
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(DirectoryDiagnosticsFileFailure {
                        file,
                        error: error.to_string(),
                    }),
            }
        }
    };
    let worker_count = max_concurrency.min(files_to_process.len());
    futures::future::join_all((0..worker_count).map(|_| worker())).await;
    manager.release_client(&root, &server.id);

    let all_diagnostics = all_diagnostics
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    let file_errors = file_errors
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    let capped_note = if was_capped {
        format!(" (capped at {max_files})")
    } else {
        String::new()
    };
    let mut lines = vec![
        format!("Directory: {abs_dir}"),
        format!("Extension: {extension}"),
        format!("Files scanned: {}{capped_note}", files_to_process.len()),
        format!("Files with errors: {}", file_errors.len()),
        format!("Total diagnostics: {}", all_diagnostics.len()),
    ];
    if !file_errors.is_empty() {
        lines.push(String::new());
        lines.push("File processing errors:".to_string());
        lines.extend(
            file_errors
                .iter()
                .map(|failure| format!("  {}: {}", failure.file, failure.error)),
        );
    }
    if !all_diagnostics.is_empty() {
        lines.push(String::new());
        lines.extend(
            all_diagnostics
                .iter()
                .take(DEFAULT_MAX_DIAGNOSTICS)
                .map(|(file, diagnostic)| format!("{file}: {}", format_diagnostic(diagnostic))),
        );
        if all_diagnostics.len() > DEFAULT_MAX_DIAGNOSTICS {
            lines.push(String::new());
            lines.push(format!(
                "... ({} more diagnostics not shown)",
                all_diagnostics.len() - DEFAULT_MAX_DIAGNOSTICS
            ));
        }
    }
    Ok(DirectoryDiagnosticsResult {
        output: lines.join("\n"),
        total_diagnostics: all_diagnostics.len(),
        file_failures: file_errors,
    })
}

#[cfg(test)]
#[path = "directory_diagnostics_tests.rs"]
mod tests;
