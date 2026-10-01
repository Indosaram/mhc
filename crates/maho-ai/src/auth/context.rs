//! Port of senpi packages/ai/src/auth/context.ts.

use crate::auth::types::AuthContext;
use async_trait::async_trait;
use std::path::PathBuf;

pub struct DefaultAuthContext;

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[async_trait]
impl AuthContext for DefaultAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        non_blank(process_env(name))
    }

    async fn file_exists(&self, path: &str) -> bool {
        let resolved = resolve_home(path);
        tokio::fs::metadata(&resolved).await.is_ok()
    }
}

fn resolve_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix('~')
        && let Some(home) = dirs::home_dir() {
            return PathBuf::from(format!("{}{}", home.display(), rest));
        }
    PathBuf::from(path)
}

pub fn default_provider_auth_context() -> DefaultAuthContext {
    DefaultAuthContext
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn env_returns_none_for_blank_or_unset() {
        assert_eq!(non_blank(Some("  ".into())), None);
        assert_eq!(non_blank(Some(String::new())), None);
        assert_eq!(non_blank(Some("value".into())), Some("value".into()));
        assert_eq!(non_blank(None), None);

        let ctx = DefaultAuthContext;
        assert_eq!(ctx.env("MAHO_AI_TEST_CONTEXT_ENV_MISSING").await, None);
        assert_eq!(ctx.env("PATH").await, std::env::var("PATH").ok());
    }

    #[tokio::test]
    async fn file_exists_resolves_home_and_reports_missing() {
        let ctx = DefaultAuthContext;
        let temp = tempfile::NamedTempFile::new().expect("tempfile");
        assert!(ctx.file_exists(temp.path().to_str().expect("utf8 path")).await);
        assert!(!ctx.file_exists("/nonexistent/path/for/maho-ai-tests").await);
    }
}
