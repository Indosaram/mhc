//! Interfaces for work that runs after a tool mutated files: formatters first, then
//! post-mutation hooks (LSP diagnostics, comment checks, ...). Each hook may return text that is
//! appended to the tool result.
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

pub type HookFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Files a tool call wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mutation {
    /// Tool name as called (`write`, `edit`, `apply_patch`, ...).
    pub tool_name: String,
    pub tool_call_id: String,
    pub cwd: PathBuf,
    /// Mutated paths as the tool reported them (relative to `cwd` or absolute).
    pub files: Vec<String>,
}

impl Mutation {
    /// `file` resolved against `cwd`.
    pub fn absolute(&self, file: &str) -> PathBuf {
        let path = Path::new(file);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }
}

/// Outcome of formatting one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatOutcome {
    Unchanged,
    Formatted,
    Failed(String),
}

/// Rewrites a mutated file in place (e.g. an external formatter or an LSP formatting request).
pub trait Formatter: Send + Sync {
    fn id(&self) -> &str;
    fn supports(&self, path: &Path) -> bool;
    fn format<'a>(&'a self, path: &'a Path) -> HookFuture<'a, FormatOutcome>;
}

/// Inspects a mutation after formatting; `Some(text)` is appended to the tool result.
pub trait PostMutationHook: Send + Sync {
    fn id(&self) -> &str;
    fn after_mutation<'a>(&'a self, mutation: &'a Mutation) -> HookFuture<'a, Option<String>>;
}

/// Formatters and hooks in registration order.
#[derive(Clone, Default)]
pub struct HookChain {
    formatters: Vec<Arc<dyn Formatter>>,
    hooks: Vec<Arc<dyn PostMutationHook>>,
}

impl HookChain {
    pub fn with_formatter(mut self, formatter: Arc<dyn Formatter>) -> Self {
        self.formatters.push(formatter);
        self
    }
    pub fn with_hook(mut self, hook: Arc<dyn PostMutationHook>) -> Self {
        self.hooks.push(hook);
        self
    }
    pub fn is_empty(&self) -> bool {
        self.formatters.is_empty() && self.hooks.is_empty()
    }

    /// Runs the first supporting formatter per file, then every hook; returns the appended texts
    /// (formatter failures included as one line each).
    pub async fn run(&self, mutation: &Mutation) -> Vec<String> {
        let mut texts = Vec::new();
        for file in &mutation.files {
            let path = mutation.absolute(file);
            if let Some(formatter) = self
                .formatters
                .iter()
                .find(|formatter| formatter.supports(&path))
                && let FormatOutcome::Failed(reason) = formatter.format(&path).await
            {
                texts.push(format!(
                    "Formatter {} failed for {file}: {reason}",
                    formatter.id()
                ));
            }
        }
        for hook in &self.hooks {
            if let Some(text) = hook
                .after_mutation(mutation)
                .await
                .filter(|text| !text.trim().is_empty())
            {
                texts.push(text);
            }
        }
        texts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Upper(Mutex<Vec<PathBuf>>);
    impl Formatter for Upper {
        fn id(&self) -> &str {
            "upper"
        }
        fn supports(&self, path: &Path) -> bool {
            path.extension().is_some_and(|ext| ext == "txt")
        }
        fn format<'a>(&'a self, path: &'a Path) -> HookFuture<'a, FormatOutcome> {
            Box::pin(async move {
                self.0.lock().expect("lock").push(path.to_path_buf());
                if path.ends_with("bad.txt") {
                    FormatOutcome::Failed("boom".into())
                } else {
                    FormatOutcome::Formatted
                }
            })
        }
    }
    struct Echo;
    impl PostMutationHook for Echo {
        fn id(&self) -> &str {
            "echo"
        }
        fn after_mutation<'a>(&'a self, mutation: &'a Mutation) -> HookFuture<'a, Option<String>> {
            Box::pin(async move {
                Some(format!(
                    "{} touched {}",
                    mutation.tool_name,
                    mutation.files.len()
                ))
            })
        }
    }

    #[tokio::test]
    async fn formatters_then_hooks() {
        let formatter = Arc::new(Upper(Mutex::new(Vec::new())));
        let chain = HookChain::default()
            .with_formatter(formatter.clone())
            .with_hook(Arc::new(Echo));
        let mutation = Mutation {
            tool_name: "write".into(),
            tool_call_id: "c".into(),
            cwd: PathBuf::from("/w"),
            files: vec!["a.txt".into(), "bad.txt".into(), "c.rs".into()],
        };
        let texts = chain.run(&mutation).await;
        assert_eq!(
            texts,
            vec![
                "Formatter upper failed for bad.txt: boom".to_owned(),
                "write touched 3".to_owned()
            ]
        );
        assert_eq!(
            formatter.0.lock().expect("lock").clone(),
            vec![PathBuf::from("/w/a.txt"), PathBuf::from("/w/bad.txt")]
        );
    }
}
