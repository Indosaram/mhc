//! Spec §5 step 1: the optional language-server formatting path.
use crate::run::{RunOutcome, line_delta};
use maho_lsp::LspError;
use maho_lsp::edit::apply_text_edits;
use maho_lsp::tools::LspTools;
use maho_lsp::types::TextEdit;
use serde_json::json;
use std::path::Path;

/// What a language server answered to a formatting request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LspAnswer {
    /// The file was reformatted with these line counts.
    Formatted { added: usize, removed: usize },
    /// The server lacks formatting support, or formatting is unavailable: use the CLI.
    Unavailable,
    /// Anything else (including success without detail): the file counts as unchanged.
    Other,
}

/// Name shown when the language server formatted the file.
pub const LSP_FORMATTER_NAME: &str = "language server";

/// Formats a file through a language server.
pub trait LspFormat: Send + Sync {
    fn format<'a>(
        &'a self,
        cwd: &'a Path,
        path: &'a Path,
    ) -> maho_lsp::hooks::HookFuture<'a, LspAnswer>;
}

impl LspAnswer {
    /// `None` = fall through to the CLI path.
    pub fn outcome(self) -> Option<RunOutcome> {
        match self {
            Self::Formatted { added, removed } => Some(RunOutcome::Formatted {
                formatter: LSP_FORMATTER_NAME.to_owned(),
                added,
                removed,
            }),
            Self::Unavailable => None,
            Self::Other => Some(RunOutcome::Unchanged),
        }
    }
}

/// `textDocument/formatting` through the shared LSP pool.
pub struct LspPoolFormatter {
    pub tools: LspTools,
}

const METHOD_NOT_FOUND: i64 = -32601;

/// A server for `path` is already running and initialized in the shared pool (formatting never
/// starts a cold server; that would delay every edit by the server's start-up).
fn server_running(tools: &LspTools, cwd: &Path, path: &Path) -> bool {
    let extension = maho_lsp::client::dotted_extension(path);
    let maho_lsp::config::ServerLookup::Found(server) =
        maho_lsp::config::find_server_for_extension(&tools.config, &extension, cwd)
    else {
        return false;
    };
    let root = maho_lsp::tools::find_workspace_root(path);
    tools.manager.snapshot().iter().any(|row| {
        row.root == root && row.server_id == server.id && row.alive && !row.initializing
    })
}

async fn request_format(tools: &LspTools, cwd: &Path, path: &Path) -> Result<Vec<TextEdit>, LspError> {
    tools
        .with_client(path, cwd, "formatting", |client| async move {
            let uri = client.open_file(path).await?;
            let reply = client
                .request(
                    "textDocument/formatting",
                    Some(json!({
                        "textDocument": {"uri": uri},
                        "options": {"tabSize": 4, "insertSpaces": true}
                    })),
                )
                .await?;
            Ok(serde_json::from_value::<Vec<TextEdit>>(reply).unwrap_or_default())
        })
        .await
}

impl LspFormat for LspPoolFormatter {
    fn format<'a>(
        &'a self,
        cwd: &'a Path,
        path: &'a Path,
    ) -> maho_lsp::hooks::HookFuture<'a, LspAnswer> {
        Box::pin(async move {
            if !server_running(&self.tools, cwd, path) {
                return LspAnswer::Unavailable;
            }
            match request_format(&self.tools, cwd, path).await {
                Err(LspError::ServerLookup(_) | LspError::InvalidPath(_)) => LspAnswer::Unavailable,
                Err(LspError::Rpc { code, .. }) if code == METHOD_NOT_FOUND => {
                    LspAnswer::Unavailable
                }
                Err(LspError::Spawn(_) | LspError::Initializing(_)) => LspAnswer::Unavailable,
                Err(_) => LspAnswer::Other,
                Ok(edits) if edits.is_empty() => LspAnswer::Other,
                Ok(edits) => {
                    let Ok(before) = std::fs::read_to_string(path) else {
                        return LspAnswer::Other;
                    };
                    let after = apply_text_edits(&before, &edits);
                    if after == before || std::fs::write(path, &after).is_err() {
                        return LspAnswer::Other;
                    }
                    let (added, removed) = line_delta(before.as_bytes(), after.as_bytes());
                    LspAnswer::Formatted { added, removed }
                }
            }
        })
    }
}

/// Never consulted: every file goes to the CLI path.
pub struct NoLsp;

impl LspFormat for NoLsp {
    fn format<'a>(
        &'a self,
        _cwd: &'a Path,
        _path: &'a Path,
    ) -> maho_lsp::hooks::HookFuture<'a, LspAnswer> {
        Box::pin(async { LspAnswer::Unavailable })
    }
}
