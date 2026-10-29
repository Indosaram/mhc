//! One tool result end to end (spec §1-§6).
use crate::MUTATION_TOOLS;
use crate::detect::{FormatterKind, Markers, prettier_or_bun};
use crate::lsp::{LspFormat, NoLsp};
use crate::paths::{collect_paths, display_relative, normalize, resolve};
use crate::run::{Exit, Host, RunOutcome, SharedRuns};
use crate::settings::{Mode, Settings, language_of};
use serde_json::Value;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// One successful tool result to format.
#[derive(Clone, Debug)]
pub struct FormatRequest {
    pub tool_name: String,
    pub input: Value,
    /// The tool result's details (the per-file list of `apply_patch`).
    pub details: Option<Value>,
    pub cwd: PathBuf,
    pub session_id: String,
    /// `None` reads `formatOnMutation` from the merged config of `cwd`.
    pub settings: Option<Settings>,
}

/// What to do with the tool result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormatReport {
    /// Text blocks to append, in file order.
    pub blocks: Vec<String>,
    /// Mark the tool result as an error (a required formatter is missing).
    pub is_error: bool,
    /// Warnings to log now (each one at most once per session and formatter).
    pub warnings: Vec<String>,
}

/// Shared state across tool results: process host, LSP path, in-flight runs, issued warnings.
pub struct FormatEngine {
    pub host: Arc<Host>,
    lsp: Arc<dyn LspFormat>,
    runs: SharedRuns,
    warned: Mutex<HashSet<(String, String)>>,
}

impl Default for FormatEngine {
    fn default() -> Self {
        Self::new(Arc::new(Host::default()), Arc::new(NoLsp))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Block appended for a formatted file.
pub fn formatted_block(path: &str, formatter: &str, added: usize, removed: usize, tool: &str) -> String {
    format!(
        "{formatter} reformatted {path} (+{added}/-{removed} lines) after {tool}; the file on disk \
         now differs from what {tool} wrote, so read it again before editing it."
    )
}

/// Text appended when a required formatter is missing.
pub fn missing_block(path: &str, formatter: &str) -> String {
    format!(
        "Formatting is required but the formatter {formatter} was not found, so {path} was not \
         formatted."
    )
}

/// Warning logged once per session when a formatter is missing in best-effort mode.
pub fn missing_warning(formatter: &str) -> String {
    format!("Formatter {formatter} not found; changed files are left unformatted.")
}

/// `git check-ignore` relative to `cwd`; any git failure means "not ignored". `git` is looked up
/// on the process `PATH`, not on the host's formatter search path.
async fn git_ignored(host: &Host, cwd: &Path, file: &Path) -> bool {
    let Some(git) = Host::default().resolve_binary(cwd, "git") else {
        return false;
    };
    let args: Vec<OsString> = vec![
        "check-ignore".into(),
        "-q".into(),
        "--".into(),
        file.as_os_str().to_owned(),
    ];
    host.run(&git, &args, cwd, GIT_TIMEOUT).await == Exit::Success
}

impl FormatEngine {
    pub fn new(host: Arc<Host>, lsp: Arc<dyn LspFormat>) -> Self {
        Self {
            host,
            lsp,
            runs: SharedRuns::default(),
            warned: Mutex::default(),
        }
    }

    /// Paths with a formatter run in flight.
    pub fn pending_runs(&self) -> usize {
        self.runs.pending()
    }

    async fn format_file(&self, kind: FormatterKind, file: &Path, cwd: &Path, timeout: Duration) -> RunOutcome {
        self.runs
            .run(file, async {
                if let Some(outcome) = self.lsp.format(cwd, file).await.outcome() {
                    return outcome;
                }
                let (program, args) = kind.command();
                self.host
                    .format_with_cli(kind.name(), program, args, file, cwd, timeout)
                    .await
            })
            .await
    }

    /// Formats every file the tool call changed.
    pub async fn process(&self, request: &FormatRequest) -> FormatReport {
        let mut report = FormatReport::default();
        if !MUTATION_TOOLS.contains(&request.tool_name.as_str()) {
            return report;
        }
        let settings = request
            .settings
            .clone()
            .unwrap_or_else(|| Settings::load(&request.cwd));
        if settings.mode == Mode::Off {
            return report;
        }
        let cwd = normalize(&request.cwd);
        let reported = collect_paths(&request.tool_name, &request.input, request.details.as_ref());
        let mut markers: Option<Markers> = None;
        let mut prettier: Option<FormatterKind> = None;
        for shown in reported {
            let file = resolve(&cwd, &shown);
            let Ok(meta) = std::fs::metadata(&file) else {
                continue;
            };
            if !meta.is_file() || meta.len() > settings.max_file_bytes {
                continue;
            }
            let Some(mut kind) = markers.get_or_insert_with(|| Markers::scan(&cwd)).choose(&file)
            else {
                continue;
            };
            let mode = settings.mode_for(&language_of(&file));
            if mode == Mode::Off || git_ignored(&self.host, &cwd, &file).await {
                continue;
            }
            if kind == FormatterKind::Prettier {
                kind = match prettier {
                    Some(kind) => kind,
                    None => {
                        let chosen = prettier_or_bun(&self.host, &cwd).await;
                        prettier = Some(chosen);
                        chosen
                    }
                };
            }
            match self.format_file(kind, &file, &cwd, settings.timeout).await {
                RunOutcome::Unchanged => {}
                RunOutcome::Formatted {
                    formatter,
                    added,
                    removed,
                } => report.blocks.push(formatted_block(
                    &display_relative(&cwd, &file),
                    &formatter,
                    added,
                    removed,
                    &request.tool_name,
                )),
                RunOutcome::Missing if mode == Mode::Required => {
                    report.is_error = true;
                    report.blocks.push(missing_block(&shown, kind.name()));
                }
                RunOutcome::Missing => {
                    let key = (request.session_id.clone(), kind.name().to_owned());
                    if lock(&self.warned).insert(key) {
                        report.warnings.push(missing_warning(kind.name()));
                    }
                }
            }
        }
        report
    }
}
