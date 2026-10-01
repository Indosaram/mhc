pub use comment_checker_core::{CheckResult,RunCommentCheckerInput};
pub type BinaryResolver=std::sync::Arc<dyn Fn()->Option<std::path::PathBuf>+Send+Sync>;
pub type CommentCheckFuture=std::pin::Pin<Box<dyn std::future::Future<Output=std::io::Result<CheckResult>>+Send>>;
pub type CommentCheck=std::sync::Arc<dyn Fn(RunCommentCheckerInput)->CommentCheckFuture+Send+Sync>;
#[derive(Default)]
pub struct CommentCheckerComponentOptions {pub resolve_binary:Option<BinaryResolver>,pub run_comment_checker:Option<CommentCheck>}
