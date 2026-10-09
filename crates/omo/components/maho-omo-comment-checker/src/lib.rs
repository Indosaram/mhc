//! maho-omo-comment-checker component

pub mod component;
pub mod constants;
pub mod downloader;
pub mod failure_notice;
pub mod hook_input;
pub mod index;
pub mod resolver;
pub mod runner;
pub mod types;
pub mod utils;

pub use component::CommentCheckerComponent;
