use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

pub use comment_checker_core::{
    CheckFailure, CheckResult, CommentCheckerReleaseAsset, RunCommentCheckerInput,
};
use maho_ext_api::ComponentLogger;

pub type BinaryResolver = Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>;
pub type CommentCheckFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<CheckResult>> + Send>>;
pub type CommentCheck = Arc<dyn Fn(RunCommentCheckerInput) -> CommentCheckFuture + Send + Sync>;

pub type DownloadBinaryFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Option<PathBuf>> + Send>>;
pub type DownloadBinary =
    Arc<dyn Fn(Option<Arc<dyn ComponentLogger>>) -> DownloadBinaryFuture + Send + Sync>;

#[derive(Default, Clone)]
pub struct CommentCheckerComponentOptions {
    pub resolve_binary: Option<BinaryResolver>,
    pub download_binary: Option<DownloadBinary>,
    pub run_comment_checker: Option<CommentCheck>,
}

pub type AssetUrlResolver = Arc<dyn Fn(&CommentCheckerReleaseAsset) -> String + Send + Sync>;

#[derive(Default, Clone)]
pub struct SenpiCommentCheckerDownloadOptions {
    pub logger: Option<Arc<dyn ComponentLogger>>,
    pub platform: Option<String>,
    pub arch: Option<String>,
    pub cache_dir: Option<PathBuf>,
    pub resolve_asset_url: Option<AssetUrlResolver>,
}

#[derive(Default, Clone)]
pub struct SenpiCommentCheckerBinaryResolverOptions {
    pub env: Option<BTreeMap<String, String>>,
    pub platform: Option<String>,
    pub cache_dir: Option<PathBuf>,
}
