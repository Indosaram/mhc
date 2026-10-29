pub mod apfs;
pub mod block_clone;
pub mod btrfs;
pub mod copy_tree;
pub mod git_fixture;
pub mod overlayfs;
pub mod rcopy;
pub mod reflink;
pub mod runtime;
pub mod zfs;

#[derive(Debug, Clone)]
pub struct NativeLoadError {
    pub code: Option<String>,
    pub message: String,
}
