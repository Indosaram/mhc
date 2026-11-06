#[cfg(feature = "omo-core")]
pub use comment_checker_core as checker;
#[cfg(not(feature = "omo-core"))]
pub use maho_comment_checker as checker;

pub mod cli;
pub mod core;
mod index;
pub mod ui;
pub use index::{CheckerRunner, CommentChecker, create_comment_checker_tool_result_handler};
