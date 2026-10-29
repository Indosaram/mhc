use std::{future::Future, path::PathBuf, pin::Pin};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelVariant {
    Default, Gpt, Gemini, Kimi, Glm, Swe2, Planner, Codex, Opus47, Minimax,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemPromptSource { pub base_dir: PathBuf }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundledPromptSource {
    pub content: &'static str,
    pub file_path: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PromptSource {
    Filesystem(FilesystemPromptSource),
    Bundled(BundledPromptSource),
}

pub type InjectionError = Box<dyn std::error::Error + Send + Sync>;
pub type InjectionFuture<'a> = Pin<Box<dyn Future<Output = Result<String, InjectionError>> + Send + 'a>>;
pub struct RuntimeInjection<'a> {
    pub placeholder: &'a str,
    pub resolver: &'a (dyn Fn() -> InjectionFuture<'static> + Sync),
}
pub struct SyncRuntimeInjection<'a> {
    pub placeholder: &'a str,
    pub resolver: &'a dyn Fn() -> Result<String, InjectionError>,
}
pub struct LoadFilesystemPromptInput<'a> {
    pub source: &'a FilesystemPromptSource,
    pub name: &'a str,
    pub variant: &'a str,
    pub inject: &'a [RuntimeInjection<'a>],
}
pub struct LoadBundledPromptInput<'a> {
    pub source: &'a BundledPromptSource,
    pub name: &'a str,
    pub variant: &'a str,
    pub inject: &'a [SyncRuntimeInjection<'a>],
}
pub enum LoadPromptInput<'a> {
    Filesystem(LoadFilesystemPromptInput<'a>),
    Bundled(LoadBundledPromptInput<'a>),
}
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedPrompt<T = serde_json::Value> {
    pub frontmatter: T,
    pub body: String,
    pub had_frontmatter: bool,
    pub parse_error: bool,
    pub file_path: PathBuf,
}

/// Insertion order determines the first matching model and fallback variant.
pub type VariantTable = indexmap::IndexMap<&'static str, PromptSource>;
pub type PromptVariant = BundledPromptSource;
