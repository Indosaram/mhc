use std::{io, path::{Component, Path, PathBuf}};
use crate::types::{LoadBundledPromptInput, LoadedPrompt, LoadPromptInput};

#[derive(Debug)]
pub struct PromptFileNotFoundError {
    pub prompt_name: String,
    pub variant: String,
    pub file_path: PathBuf,
    pub cause: io::Error,
}
impl std::fmt::Display for PromptFileNotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Prompt file not found for {}/{}: {}", self.prompt_name, self.variant, self.file_path.display())
    }
}
impl std::error::Error for PromptFileNotFoundError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(&self.cause) }
}

#[derive(Debug)]
pub struct PromptPathTraversalError {
    pub prompt_name: String,
    pub variant: String,
}
impl std::fmt::Display for PromptPathTraversalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Prompt path escapes source directory for {}/{}", self.prompt_name, self.variant)
    }
}
impl std::error::Error for PromptPathTraversalError {}

#[derive(Debug)]
pub enum PromptLoadError {
    NotFound(PromptFileNotFoundError),
    PathTraversal(PromptPathTraversalError),
    Io(io::Error),
    Injection(crate::types::InjectionError),
}
impl std::fmt::Display for PromptLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(error) => error.fmt(f),
            Self::PathTraversal(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
            Self::Injection(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for PromptLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFound(error) => Some(error),
            Self::PathTraversal(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Injection(error) => Some(error.as_ref()),
        }
    }
}

pub async fn load_prompt(input: LoadPromptInput<'_>) -> Result<LoadedPrompt, PromptLoadError> {
    match input {
        LoadPromptInput::Bundled(input) => load_prompt_sync(input),
        LoadPromptInput::Filesystem(input) => {
            let file_path = resolve_prompt_file_path(&input.source.base_dir, input.name, input.variant)?;
            let content = tokio::fs::read_to_string(&file_path).await.map_err(|cause| {
                if cause.kind() == io::ErrorKind::NotFound {
                    PromptLoadError::NotFound(PromptFileNotFoundError {
                        prompt_name: input.name.into(), variant: input.variant.into(),
                        file_path: file_path.clone(), cause,
                    })
                } else { PromptLoadError::Io(cause) }
            })?;
            let mut loaded = parse(&content, file_path);
            for injection in input.inject {
                let value = (injection.resolver)().await.map_err(PromptLoadError::Injection)?;
                loaded.body = loaded.body.replace(injection.placeholder, &value);
            }
            Ok(loaded)
        }
    }
}

pub fn load_prompt_sync(input: LoadBundledPromptInput<'_>) -> Result<LoadedPrompt, PromptLoadError> {
    let mut loaded = parse(input.source.content, input.source.file_path.into());
    for injection in input.inject {
        let value = (injection.resolver)().map_err(PromptLoadError::Injection)?;
        loaded.body = loaded.body.replace(injection.placeholder, &value);
    }
    Ok(loaded)
}

fn parse(content: &str, file_path: PathBuf) -> LoadedPrompt {
    let parsed = utils::parse_frontmatter(content, utils::FrontmatterMode::Default);
    LoadedPrompt {
        frontmatter: parsed.data, body: parsed.body,
        had_frontmatter: parsed.had_frontmatter, parse_error: parsed.parse_error, file_path,
    }
}

fn resolve_prompt_file_path(base: &Path, name: &str, variant: &str) -> Result<PathBuf, PromptLoadError> {
    let cwd = std::env::current_dir().map_err(PromptLoadError::Io)?;
    let base = normalize(&cwd.join(base));
    let file = normalize(&base.join(name).join(format!("{variant}.md")));
    // Upstream rejects any relative path beginning with "..", including "..name".
    if !file.strip_prefix(&base).is_ok_and(|relative| !relative.to_string_lossy().starts_with("..")) {
        return Err(PromptLoadError::PathTraversal(PromptPathTraversalError {
            prompt_name: name.into(), variant: variant.into(),
        }));
    }
    Ok(file)
}

fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => { result.pop(); }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => result.push(part),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn fixture() -> FilesystemPromptSource {
        FilesystemPromptSource { base_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("src/__test_fixtures__") }
    }

    #[tokio::test]
    async fn filesystem_frontmatter_verbatim_body_and_empty_metadata() {
        let source = fixture();
        let loaded = load_prompt(LoadPromptInput::Filesystem(LoadFilesystemPromptInput {
            source: &source, name: "test-prompt", variant: "default", inject: &[],
        })).await.expect("fixture loads");
        assert_eq!(loaded.body, "Default prompt body with {X}.\nSecond line remains verbatim.\n");
        assert_eq!(loaded.frontmatter["title"], "Test Prompt");
        assert_eq!(loaded.frontmatter["enabled"], true);
        assert!(loaded.had_frontmatter);
        assert!(!loaded.parse_error);
        let empty = load_prompt(LoadPromptInput::Filesystem(LoadFilesystemPromptInput {
            source: &source, name: "test-prompt", variant: "gpt", inject: &[],
        })).await.expect("empty metadata loads");
        assert_eq!(empty.frontmatter, serde_json::json!({}));
        assert_eq!(empty.body, "GPT prompt body with {A} and {B}.\n");
    }

    #[tokio::test]
    async fn missing_file_and_both_traversal_inputs() {
        let source = fixture();
        for (name, variant) in [("test-prompt", "missing"), ("../test-prompt", "default"), ("test-prompt", "../../outside")] {
            let error = load_prompt(LoadPromptInput::Filesystem(LoadFilesystemPromptInput {
                source: &source, name, variant, inject: &[],
            })).await.expect_err("invalid request fails");
            if variant == "missing" {
                assert!(matches!(&error, PromptLoadError::NotFound(_)));
                assert!(error.to_string().contains("test-prompt/missing"));
            } else { assert!(matches!(error, PromptLoadError::PathTraversal(_))); }
        }
    }

    #[tokio::test]
    async fn filesystem_injections_are_sequential_and_propagate_errors() {
        let source = fixture();
        let alpha = || -> InjectionFuture<'static> { Box::pin(async { Ok("Alpha".into()) }) };
        let beta = || -> InjectionFuture<'static> { Box::pin(async { Ok("Beta".into()) }) };
        let absent = || -> InjectionFuture<'static> { Box::pin(async { Ok("No-op".into()) }) };
        let inject = [RuntimeInjection {placeholder: "{A}", resolver: &alpha}, RuntimeInjection {placeholder: "{B}", resolver: &beta}, RuntimeInjection {placeholder: "{ABSENT}", resolver: &absent}];
        let loaded = load_prompt(LoadPromptInput::Filesystem(LoadFilesystemPromptInput {
            source: &source, name: "test-prompt", variant: "gpt", inject: &inject,
        })).await.expect("injections resolve");
        assert_eq!(loaded.body, "GPT prompt body with Alpha and Beta.\n");
        let y = || -> InjectionFuture<'static> { Box::pin(async { Ok("Y".into()) }) };
        let inject = [RuntimeInjection {placeholder: "{X}", resolver: &y}];
        let loaded = load_prompt(LoadPromptInput::Filesystem(LoadFilesystemPromptInput {
            source: &source, name: "test-prompt", variant: "default", inject: &inject,
        })).await.expect("single injection resolves");
        assert_eq!(loaded.body, "Default prompt body with Y.\nSecond line remains verbatim.\n");
        let failure = || -> InjectionFuture<'static> { Box::pin(async { Err(io::Error::other("resolver failed").into()) }) };
        let inject = [RuntimeInjection {placeholder: "{X}", resolver: &failure}];
        let error = load_prompt(LoadPromptInput::Filesystem(LoadFilesystemPromptInput {
            source: &source, name: "test-prompt", variant: "default", inject: &inject,
        })).await.expect_err("resolver error propagates");
        assert!(matches!(&error, PromptLoadError::Injection(_)));
        assert_eq!(error.to_string(), "resolver failed");
    }

    #[tokio::test]
    async fn bundled_loaders_apply_all_or_single_injections() {
        let source = BundledPromptSource {content: "Bundled prompt body with {A}, {B}, and {C}.\n", file_path: "packages/prompts-core/prompts/test/default.md"};
        let alpha = || Ok("Alpha".into());
        let beta = || Ok("Beta".into());
        let gamma = || Ok("Gamma".into());
        let inject = [SyncRuntimeInjection {placeholder: "{A}", resolver: &alpha}, SyncRuntimeInjection {placeholder: "{B}", resolver: &beta}, SyncRuntimeInjection {placeholder: "{C}", resolver: &gamma}];
        let loaded = load_prompt(LoadPromptInput::Bundled(LoadBundledPromptInput {source: &source, name: "test-prompt", variant: "default", inject: &inject})).await.expect("bundled load");
        assert_eq!(loaded.body, "Bundled prompt body with Alpha, Beta, and Gamma.\n");
        assert_eq!(loaded.file_path, PathBuf::from(source.file_path));
        let loaded = load_prompt_sync(LoadBundledPromptInput {source: &source, name: "test-prompt", variant: "default", inject: &inject[..1]}).expect("sync load");
        assert_eq!(loaded.body, "Bundled prompt body with Alpha, {B}, and {C}.\n");
        assert_eq!(loaded.file_path, PathBuf::from(source.file_path));
    }
}
