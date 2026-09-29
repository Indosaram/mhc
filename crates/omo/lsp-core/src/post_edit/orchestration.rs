use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::sync::Mutex;
use std::sync::PoisonError;

const DEFAULT_MAX_CONCURRENCY: usize = 4;
const CLEAN_DIAGNOSTICS_TEXT: &str = "No diagnostics found";

/// TS `PostEditDiagnosticsOutcome`: rendered diagnostics text or a structured
/// `not_configured` marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostEditDiagnosticsOutcome {
    Text(String),
    NotConfigured { extension: String },
}

/// A thrown runner failure. `message` mirrors `Error.message`; an empty message falls
/// back to `Error` like TS `String(error)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsRunnerError {
    pub message: String,
}

impl DiagnosticsRunnerError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostEditDiagnosticsBlock {
    pub file_path: String,
    pub diagnostics: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostEditObservationKind {
    Block,
    Clean,
    NotConfigured,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostEditDiagnosticsObservation {
    pub file_path: String,
    pub kind: PostEditObservationKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PostEditDiagnosticsResult {
    pub blocks: Vec<PostEditDiagnosticsBlock>,
    pub observations: Vec<PostEditDiagnosticsObservation>,
}

/// TS `PostEditNotConfiguredCache`: extensions known to have no configured server.
#[derive(Debug, Default)]
pub struct PostEditNotConfiguredCache {
    not_configured_extensions: Mutex<Vec<String>>,
}

impl PostEditNotConfiguredCache {
    pub fn not_configured_extensions(&self) -> Vec<String> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        self.not_configured_extensions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn contains(&self, extension: &str) -> bool {
        self.lock().iter().any(|known| known == extension)
    }

    fn add(&self, extension: String) {
        let mut known = self.lock();
        if !known.contains(&extension) {
            known.push(extension);
        }
    }
}

pub fn create_post_edit_not_configured_cache() -> PostEditNotConfiguredCache {
    PostEditNotConfiguredCache::default()
}

pub fn reset_post_edit_not_configured_cache(cache: &PostEditNotConfiguredCache) {
    cache.lock().clear();
}

/// TS `CollectPostEditDiagnosticsInput`.
pub struct CollectPostEditDiagnosticsInput<'a, F> {
    pub file_paths: &'a [String],
    pub run_diagnostics: F,
    pub cache: Option<&'a PostEditNotConfiguredCache>,
    pub max_concurrency: Option<usize>,
}

/// TS `collectPostEditDiagnostics`: dedupes paths, skips cached not-configured
/// extensions, runs at most `max_concurrency` diagnostics at once, keeps input order.
pub async fn collect_post_edit_diagnostics<F, Fut>(
    input: CollectPostEditDiagnosticsInput<'_, F>,
) -> PostEditDiagnosticsResult
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<PostEditDiagnosticsOutcome, DiagnosticsRunnerError>>,
{
    let local_cache = PostEditNotConfiguredCache::default();
    let cache = input.cache.unwrap_or(&local_cache);
    let targets = first_seen_diagnostic_targets(input.file_paths, cache);
    let outcomes =
        run_bounded_diagnostics(&targets, &input.run_diagnostics, input.max_concurrency).await;

    let mut result = PostEditDiagnosticsResult::default();
    for (file_path, outcome) in targets.into_iter().zip(outcomes) {
        let kind = match outcome {
            PostEditDiagnosticsOutcome::NotConfigured { extension } => {
                cache.add(extension);
                PostEditObservationKind::NotConfigured
            }
            PostEditDiagnosticsOutcome::Text(text)
                if text.is_empty() || text == CLEAN_DIAGNOSTICS_TEXT =>
            {
                PostEditObservationKind::Clean
            }
            PostEditDiagnosticsOutcome::Text(diagnostics) => {
                result.blocks.push(PostEditDiagnosticsBlock {
                    file_path: file_path.clone(),
                    diagnostics,
                });
                PostEditObservationKind::Block
            }
        };
        result
            .observations
            .push(PostEditDiagnosticsObservation { file_path, kind });
    }
    result
}

fn first_seen_diagnostic_targets(
    file_paths: &[String],
    cache: &PostEditNotConfiguredCache,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for file_path in file_paths {
        if file_path.is_empty() || !seen.insert(file_path.as_str()) {
            continue;
        }
        if extension_key(file_path).is_some_and(|extension| cache.contains(&extension)) {
            continue;
        }
        targets.push(file_path.clone());
    }
    targets
}

async fn run_bounded_diagnostics<F, Fut>(
    targets: &[String],
    run_diagnostics: &F,
    max_concurrency: Option<usize>,
) -> Vec<PostEditDiagnosticsOutcome>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<PostEditDiagnosticsOutcome, DiagnosticsRunnerError>>,
{
    let worker_count = max_concurrency
        .unwrap_or(DEFAULT_MAX_CONCURRENCY)
        .max(1)
        .min(targets.len());
    let next_index = Mutex::new(0usize);
    let results: Mutex<Vec<Option<PostEditDiagnosticsOutcome>>> =
        Mutex::new(vec![None; targets.len()]);
    let worker = || async {
        loop {
            let index = {
                let mut next = next_index.lock().unwrap_or_else(PoisonError::into_inner);
                let index = *next;
                *next += 1;
                index
            };
            let Some(file_path) = targets.get(index) else {
                return;
            };
            let outcome = match run_diagnostics(file_path.clone()).await {
                Ok(PostEditDiagnosticsOutcome::Text(text)) => {
                    PostEditDiagnosticsOutcome::Text(text.trim().to_string())
                }
                Ok(not_configured) => not_configured,
                Err(error) => {
                    let message = error.message.trim();
                    PostEditDiagnosticsOutcome::Text(if message.is_empty() {
                        "Error".to_string()
                    } else {
                        message.to_string()
                    })
                }
            };
            results.lock().unwrap_or_else(PoisonError::into_inner)[index] = Some(outcome);
        }
    };
    futures::future::join_all((0..worker_count).map(|_| worker())).await;
    results
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .into_iter()
        .flatten()
        .collect()
}

fn extension_key(file_path: &str) -> Option<String> {
    let name = Path::new(file_path).file_name()?.to_string_lossy();
    let dot = name.rfind('.').filter(|&dot| dot > 0)?;
    Some(name[dot..].to_lowercase())
}

#[cfg(test)]
#[path = "orchestration_tests.rs"]
mod tests;
