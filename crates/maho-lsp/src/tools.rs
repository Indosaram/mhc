//! The six LSP tool operations: text for the model plus JSON details for renderers.
use crate::client::{LspClient, absolute, dotted_extension};
use crate::config::{ConfigPaths, ServerLookup, find_server_for_extension, format_lookup_error};
use crate::edit::apply_workspace_edit;
use crate::error::{LspError, missing_dependency_message};
use crate::format::{
    filter_by_severity, format_apply_result, format_diagnostic, format_document_symbol,
    format_location, format_prepare_rename, format_symbol_info,
};
use crate::limits::{MAX_DIAGNOSTICS, MAX_DIRECTORY_FILES, MAX_REFERENCES, MAX_SYMBOLS};
use crate::manager::LspManager;
use crate::servers::EXT_TO_LANG;
use crate::types::{AnyLocation, AnySymbol, Diagnostic, SeverityFilter, SymbolInfo};
use serde_json::{Value, json};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Text + details of one tool call.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput {
    pub text: String,
    pub details: Value,
}

const SKIP_DIRECTORIES: [&str; 6] = ["node_modules", ".git", "dist", "build", ".next", "out"];
const WORKSPACE_MARKERS: [&str; 7] = [
    ".git",
    "package.json",
    "pyproject.toml",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
];
const MAX_SCAN_ENTRIES: usize = 500;

/// Nearest ancestor holding a workspace marker, else the file's directory.
pub fn find_workspace_root(path: &Path) -> PathBuf {
    let start = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    for directory in start.ancestors() {
        if WORKSPACE_MARKERS
            .iter()
            .any(|marker| directory.join(marker).exists())
        {
            return directory.to_path_buf();
        }
    }
    path.parent().map(Path::to_path_buf).unwrap_or(start)
}

fn walk(directory: &Path, visit: &mut dyn FnMut(&Path) -> bool) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut names: Vec<_> = entries.filter_map(Result::ok).collect();
    names.sort_by_key(std::fs::DirEntry::file_name);
    for entry in names {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if !visit(&path) {
            return;
        }
        if meta.is_dir()
            && !SKIP_DIRECTORIES.contains(&entry.file_name().to_string_lossy().as_ref())
        {
            walk(&path, visit);
        }
    }
}

/// Most common known source extension among the first 500 entries.
pub fn infer_extension_from_directory(directory: &Path) -> Option<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut scanned = 0;
    walk(directory, &mut |path| {
        if scanned >= MAX_SCAN_ENTRIES {
            return false;
        }
        scanned += 1;
        if path.is_file() {
            let ext = dotted_extension(path);
            if !ext.is_empty() && EXT_TO_LANG.iter().any(|(known, _)| *known == ext) {
                match counts.iter_mut().find(|(known, _)| *known == ext) {
                    Some((_, count)) => *count += 1,
                    None => counts.push((ext, 1)),
                }
            }
        }
        true
    });
    let mut best: Option<(String, usize)> = None;
    for (ext, count) in counts {
        if best.as_ref().is_none_or(|(_, top)| count > *top) {
            best = Some((ext, count));
        }
    }
    best.map(|(ext, _)| ext)
}

fn collect_files(directory: &Path, extension: &str, max: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk(directory, &mut |path| {
        if files.len() >= max {
            return false;
        }
        if path.is_file() && dotted_extension(path) == extension {
            files.push(path.to_path_buf());
        }
        true
    });
    files
}

/// Runs LSP operations against a shared [`LspManager`].
#[derive(Clone)]
pub struct LspTools {
    pub manager: Arc<LspManager>,
    pub config: ConfigPaths,
}

const READ_ONLY_RETRY: [&str; 6] = [
    "diagnostics",
    "definition",
    "references",
    "documentSymbols",
    "workspaceSymbols",
    "prepareRename",
];

impl LspTools {
    pub fn new(manager: Arc<LspManager>, config: ConfigPaths) -> Self {
        Self { manager, config }
    }

    fn resolve(&self, cwd: &Path, file_path: &str) -> PathBuf {
        let path = Path::new(file_path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            absolute(&cwd.join(path))
        }
    }

    fn lookup(
        &self,
        extension: &str,
        cwd: &Path,
    ) -> Result<crate::config::ResolvedServer, LspError> {
        match find_server_for_extension(&self.config, extension, cwd) {
            ServerLookup::Found(server) => Ok(server),
            other => Err(LspError::ServerLookup(format_lookup_error(&other))),
        }
    }

    /// Acquires the client for `path`, runs `operation`, retries once on a dead connection for
    /// read-only operations, and reports a timeout during initialization as "still initializing".
    pub async fn with_client<T, F, Fut>(
        &self,
        path: &Path,
        cwd: &Path,
        operation_name: &str,
        operation: F,
    ) -> Result<T, LspError>
    where
        F: Fn(Arc<LspClient>) -> Fut,
        Fut: Future<Output = Result<T, LspError>>,
    {
        if path.is_dir() {
            return Err(LspError::InvalidPath(
                "Directory paths are not supported by this LSP tool. Use lsp_diagnostics with a directory path for directory diagnostics."
                    .to_owned(),
            ));
        }
        let server = self.lookup(&dotted_extension(path), cwd)?;
        let root = find_workspace_root(path);
        let mut allow_retry = true;
        loop {
            let client = self.manager.get_client(&root, &server).await?;
            let outcome = operation(Arc::clone(&client)).await;
            self.manager.release_client(&root, &server.id);
            match outcome {
                Err(error)
                    if allow_retry
                        && READ_ONLY_RETRY.contains(&operation_name)
                        && error.is_dead_connection() =>
                {
                    self.manager
                        .invalidate_client(&root, &server.id, Some(&client));
                    allow_retry = false;
                }
                Err(error @ LspError::RequestTimeout { .. })
                    if self.manager.is_server_initializing(&root, &server.id) =>
                {
                    return Err(LspError::Initializing(error.to_string()));
                }
                other => return other,
            }
        }
    }

    /// `lsp_diagnostics` for a file or a directory.
    pub async fn diagnostics(
        &self,
        cwd: &Path,
        file_path: &str,
        severity: Option<&str>,
    ) -> Result<ToolOutput, LspError> {
        let filter = SeverityFilter::parse(severity);
        let path = self.resolve(cwd, file_path);
        let base = |mode: &str| json!({"filePath": file_path, "severity": filter.as_str(), "mode": mode, "diagnostics": [], "totalDiagnostics": 0, "truncated": false});
        let outcome: Result<ToolOutput, LspError> = async {
            if path.is_dir() {
                let Some(extension) = infer_extension_from_directory(&path) else {
                    let text = format!("No supported source files found in directory: {}", path.display());
                    let mut details = base("directory");
                    details["error"] = json!(text);
                    details["errorKind"] = json!("no_files");
                    return Ok(ToolOutput { text, details });
                };
                let text = self.directory_diagnostics(cwd, &path, &extension, filter, MAX_DIRECTORY_FILES).await?;
                return Ok(ToolOutput { text, details: base("directory") });
            }
            let diagnostics = self.with_client(&path, cwd, "diagnostics", |client| {
                let path = path.clone();
                async move { client.diagnostics(&path).await }
            }).await?;
            let diagnostics = filter_by_severity(diagnostics, filter);
            let total = diagnostics.len();
            let truncated = total > MAX_DIAGNOSTICS;
            let text = if total == 0 {
                "No diagnostics found".to_owned()
            } else {
                let mut lines: Vec<String> = diagnostics.iter().take(MAX_DIAGNOSTICS).map(format_diagnostic).collect();
                if truncated {
                    lines.insert(0, format!("Found {total} diagnostics (showing first {MAX_DIAGNOSTICS}):"));
                }
                lines.join("\n")
            };
            let mut details = base("file");
            details["diagnostics"] = json!(diagnostics.iter().map(|diagnostic| json!({"file": path.display().to_string(), "diagnostic": diagnostic})).collect::<Vec<_>>());
            details["totalDiagnostics"] = json!(total);
            details["truncated"] = json!(truncated);
            Ok(ToolOutput { text, details })
        }
        .await;
        recover(outcome, || base("file"))
    }

    /// Directory report: diagnostics of up to `max_files` files with `extension`.
    pub async fn directory_diagnostics(
        &self,
        cwd: &Path,
        directory: &Path,
        extension: &str,
        filter: SeverityFilter,
        max_files: usize,
    ) -> Result<String, LspError> {
        if !extension.starts_with('.') {
            return Err(LspError::InvalidPath(format!(
                "Extension must start with a dot (e.g., \".ts\", not \"{extension}\"). Use \".{extension}\" instead."
            )));
        }
        if !directory.exists() {
            return Err(LspError::InvalidPath(format!(
                "Directory does not exist: {}",
                directory.display()
            )));
        }
        let server = self.lookup(extension, cwd)?;
        let mut files = collect_files(directory, extension, max_files + 1);
        let capped = files.len() > max_files;
        files.truncate(max_files);
        if files.is_empty() {
            return Ok([
                format!("Directory: {}", directory.display()),
                format!("Extension: {extension}"),
                "Files scanned: 0".to_owned(),
                format!("No files found with extension \"{extension}\"."),
            ]
            .join("\n"));
        }
        let root = find_workspace_root(directory);
        let client = self.manager.get_client(&root, &server).await?;
        let mut found: Vec<(PathBuf, Diagnostic)> = Vec::new();
        let mut errors: Vec<(PathBuf, String)> = Vec::new();
        for file in &files {
            match client.diagnostics(file).await {
                Ok(items) => found.extend(
                    filter_by_severity(items, filter)
                        .into_iter()
                        .map(|diagnostic| (file.clone(), diagnostic)),
                ),
                Err(error) => errors.push((file.clone(), error.to_string())),
            }
        }
        self.manager.release_client(&root, &server.id);
        let mut lines = vec![
            format!("Directory: {}", directory.display()),
            format!("Extension: {extension}"),
            format!(
                "Files scanned: {}{}",
                files.len(),
                if capped {
                    format!(" (capped at {max_files})")
                } else {
                    String::new()
                }
            ),
            format!("Files with errors: {}", errors.len()),
            format!("Total diagnostics: {}", found.len()),
        ];
        if !errors.is_empty() {
            lines.push(String::new());
            lines.push("File processing errors:".to_owned());
            lines.extend(
                errors
                    .iter()
                    .map(|(file, error)| format!("  {}: {error}", file.display())),
            );
        }
        if !found.is_empty() {
            lines.push(String::new());
            lines.extend(
                found
                    .iter()
                    .take(MAX_DIAGNOSTICS)
                    .map(|(file, diagnostic)| {
                        format!("{}: {}", file.display(), format_diagnostic(diagnostic))
                    }),
            );
            if found.len() > MAX_DIAGNOSTICS {
                lines.push(String::new());
                lines.push(format!(
                    "... ({} more diagnostics not shown)",
                    found.len() - MAX_DIAGNOSTICS
                ));
            }
        }
        Ok(lines.join("\n"))
    }

    /// `lsp_goto_definition`.
    pub async fn goto_definition(
        &self,
        cwd: &Path,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<ToolOutput, LspError> {
        let path = self.resolve(cwd, file_path);
        let base = || json!({"filePath": file_path, "line": line, "character": character, "locations": []});
        let outcome = async {
            let result = self
                .with_client(&path, cwd, "definition", |client| {
                    let path = path.clone();
                    async move { client.definition(&path, line, character).await }
                })
                .await?;
            let locations: Vec<AnyLocation> = match result {
                Value::Null => Vec::new(),
                Value::Array(items) => items
                    .into_iter()
                    .filter_map(|item| serde_json::from_value(item).ok())
                    .collect(),
                single => serde_json::from_value(single).ok().into_iter().collect(),
            };
            let mut details = base();
            details["locations"] = json!(locations);
            let text = if locations.is_empty() {
                "No definition found".to_owned()
            } else {
                locations
                    .iter()
                    .map(format_location)
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(ToolOutput { text, details })
        }
        .await;
        recover(outcome, base)
    }

    /// `lsp_find_references`.
    pub async fn find_references(
        &self,
        cwd: &Path,
        file_path: &str,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> Result<ToolOutput, LspError> {
        let path = self.resolve(cwd, file_path);
        let base = || json!({"filePath": file_path, "line": line, "character": character, "references": [], "totalReferences": 0, "truncated": false});
        let outcome = async {
            let result = self
                .with_client(&path, cwd, "references", |client| {
                    let path = path.clone();
                    async move {
                        client
                            .references(&path, line, character, include_declaration)
                            .await
                    }
                })
                .await?;
            let all: Vec<AnyLocation> = result
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| serde_json::from_value(item.clone()).ok())
                        .collect()
                })
                .unwrap_or_default();
            let total = all.len();
            let truncated = total > MAX_REFERENCES;
            let text = if total == 0 {
                "No references found".to_owned()
            } else {
                let mut lines: Vec<String> = all
                    .iter()
                    .take(MAX_REFERENCES)
                    .map(format_location)
                    .collect();
                if truncated {
                    lines.insert(
                        0,
                        format!("Found {total} references (showing first {MAX_REFERENCES}):"),
                    );
                }
                lines.join("\n")
            };
            let mut details = base();
            details["references"] = json!(all);
            details["totalReferences"] = json!(total);
            details["truncated"] = json!(truncated);
            Ok(ToolOutput { text, details })
        }
        .await;
        recover(outcome, base)
    }

    /// `lsp_symbols` (`scope` = `document` | `workspace`).
    pub async fn symbols(
        &self,
        cwd: &Path,
        file_path: &str,
        scope: &str,
        query: Option<&str>,
        limit: Option<usize>,
    ) -> Result<ToolOutput, LspError> {
        let path = self.resolve(cwd, file_path);
        let workspace = scope == "workspace";
        let scope_name = if workspace { "workspace" } else { "document" };
        let base = || {
            let mut details = json!({"filePath": file_path, "scope": scope_name, "symbols": [], "totalSymbols": 0, "truncated": false});
            if let Some(query) = query {
                details["query"] = json!(query);
            }
            details
        };
        let limit = limit.unwrap_or(MAX_SYMBOLS).min(MAX_SYMBOLS);
        let outcome = async {
            if workspace {
                let Some(query) = query.filter(|query| !query.is_empty()) else {
                    let text = "Error: 'query' is required for workspace scope".to_owned();
                    let mut details = base();
                    details["error"] = json!(text);
                    details["errorKind"] = json!("missing_query");
                    return Ok(ToolOutput { text, details });
                };
                let result = self
                    .with_client(&path, cwd, "workspaceSymbols", |client| {
                        let query = query.to_owned();
                        async move { client.workspace_symbols(&query).await }
                    })
                    .await?;
                let all: Vec<SymbolInfo> = result
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| serde_json::from_value(item.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                let total = all.len();
                let truncated = total > limit;
                let text = if total == 0 {
                    "No symbols found".to_owned()
                } else {
                    let mut lines: Vec<String> =
                        all.iter().take(limit).map(format_symbol_info).collect();
                    if truncated {
                        lines.insert(0, format!("Found {total} symbols (showing first {limit}):"));
                    }
                    lines.join("\n")
                };
                let mut details = base();
                details["symbols"] = json!(all);
                details["totalSymbols"] = json!(total);
                details["truncated"] = json!(truncated);
                return Ok(ToolOutput { text, details });
            }
            let result = self
                .with_client(&path, cwd, "documentSymbols", |client| {
                    let path = path.clone();
                    async move { client.document_symbols(&path).await }
                })
                .await?;
            let all: Vec<AnySymbol> = result
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| serde_json::from_value(item.clone()).ok())
                        .collect()
                })
                .unwrap_or_default();
            let total = all.len();
            let truncated = total > limit;
            let text = if total == 0 {
                "No symbols found".to_owned()
            } else {
                let limited: Vec<&AnySymbol> = all.iter().take(limit).collect();
                let mut lines = Vec::new();
                if truncated {
                    lines.push(format!("Found {total} symbols (showing first {limit}):"));
                }
                if limited
                    .iter()
                    .all(|symbol| matches!(symbol, AnySymbol::Document(_)))
                {
                    lines.extend(limited.iter().filter_map(|symbol| match symbol {
                        AnySymbol::Document(symbol) => Some(format_document_symbol(symbol, 0)),
                        AnySymbol::Info(_) => None,
                    }));
                } else {
                    lines.extend(limited.iter().filter_map(|symbol| match symbol {
                        AnySymbol::Info(symbol) => Some(format_symbol_info(symbol)),
                        AnySymbol::Document(_) => None,
                    }));
                }
                lines.join("\n")
            };
            let mut details = base();
            details["symbols"] = json!(all);
            details["totalSymbols"] = json!(total);
            details["truncated"] = json!(truncated);
            Ok(ToolOutput { text, details })
        }
        .await;
        recover(outcome, base)
    }

    /// `lsp_prepare_rename`.
    pub async fn prepare_rename(
        &self,
        cwd: &Path,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<ToolOutput, LspError> {
        let path = self.resolve(cwd, file_path);
        let base =
            || json!({"filePath": file_path, "line": line, "character": character, "result": null});
        let outcome = async {
            let result = self
                .with_client(&path, cwd, "prepareRename", |client| {
                    let path = path.clone();
                    async move { client.prepare_rename(&path, line, character).await }
                })
                .await?;
            let mut details = base();
            let text = format_prepare_rename(&result);
            details["result"] = result;
            Ok(ToolOutput { text, details })
        }
        .await;
        recover(outcome, base)
    }

    /// `lsp_rename`: asks for the edit and applies it.
    pub async fn rename(
        &self,
        cwd: &Path,
        file_path: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<ToolOutput, LspError> {
        let path = self.resolve(cwd, file_path);
        let base = || json!({"filePath": file_path, "line": line, "character": character, "newName": new_name, "apply": null, "edit": null});
        let outcome = async {
            let edit = self
                .with_client(&path, cwd, "rename", |client| {
                    let path = path.clone();
                    let new_name = new_name.to_owned();
                    async move { client.rename(&path, line, character, &new_name).await }
                })
                .await?;
            let applied = apply_workspace_edit(&edit);
            let mut details = base();
            details["apply"] = json!(applied);
            details["edit"] = edit;
            Ok(ToolOutput {
                text: format_apply_result(&applied),
                details,
            })
        }
        .await;
        recover(outcome, base)
    }
}

/// Missing-server failures become a normal result with `errorKind: missing_dependency`.
fn recover(
    outcome: Result<ToolOutput, LspError>,
    base: impl Fn() -> Value,
) -> Result<ToolOutput, LspError> {
    match outcome {
        Err(error) => match missing_dependency_message(&error) {
            Some(text) => {
                let mut details = base();
                details["error"] = json!(text);
                details["errorKind"] = json!("missing_dependency");
                Ok(ToolOutput { text, details })
            }
            None => Err(error),
        },
        ok => ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_root_and_extension_inference() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("pkg/src/node_modules")).expect("mkdir");
        std::fs::write(dir.path().join("pkg/Cargo.toml"), "").expect("marker");
        std::fs::write(dir.path().join("pkg/src/a.rs"), "").expect("a");
        std::fs::write(dir.path().join("pkg/src/b.rs"), "").expect("b");
        std::fs::write(dir.path().join("pkg/src/c.py"), "").expect("c");
        std::fs::write(dir.path().join("pkg/src/node_modules/x.py"), "").expect("skipped");
        std::fs::write(dir.path().join("pkg/src/node_modules/y.py"), "").expect("skipped");
        assert_eq!(
            find_workspace_root(&dir.path().join("pkg/src/a.rs")),
            dir.path().join("pkg")
        );
        assert_eq!(
            infer_extension_from_directory(&dir.path().join("pkg")).as_deref(),
            Some(".rs")
        );
        assert_eq!(collect_files(&dir.path().join("pkg"), ".py", 10).len(), 1);
    }

    #[tokio::test]
    async fn missing_server_is_a_result_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("a.unknownlang");
        std::fs::write(&file, "x").expect("write");
        let tools = LspTools::new(LspManager::new(Default::default()), ConfigPaths::default());
        let output = tools
            .goto_definition(dir.path(), "a.unknownlang", 1, 0)
            .await
            .expect("result");
        assert!(
            output
                .text
                .starts_with("No LSP server configured for extension: .unknownlang")
        );
        assert_eq!(output.details["errorKind"], "missing_dependency");
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).expect("mkdir");
        let output = tools
            .diagnostics(dir.path(), "empty", None)
            .await
            .expect("dir result");
        assert_eq!(output.details["errorKind"], "no_files");
        let error = tools
            .symbols(dir.path(), "empty", "document", None, None)
            .await
            .expect_err("dir path");
        assert!(matches!(error, LspError::InvalidPath(_)));
    }
}
