#[cfg(feature = "omo-core")]
pub use comment_checker_core as checker;
#[cfg(not(feature = "omo-core"))]
pub use maho_comment_checker as checker;

pub mod core;
pub mod cli;
pub mod ui;
mod index;
pub use index::{CommentChecker, CheckerRunner, create_comment_checker_tool_result_handler};
