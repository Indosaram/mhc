//! EINTR-resilient filesystem boundary for the memory stack.

pub mod rename_contention;
pub mod resilient;
pub mod retry;
pub mod write_all;

pub use rename_contention::{
    CONTENTION_DELAYS_MS, RenamePlatform, is_contention_error, rename_with_contention_retry,
};
pub use resilient::{
    append, close_sync, create_dir_all, create_exclusive, exists, metadata, read,
    read_dir_directories, read_dir_names, read_to_string, remove_dir_all, remove_file, rename,
    sleep_ms, write,
};
pub use retry::{EINTR_RETRY_CAP, is_eintr, retry_on_eintr};
pub use write_all::{is_exclusive_flag, open_with_exclusive_policy, write_all_to_handle, write_path_all};
