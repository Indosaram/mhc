use std::path::PathBuf;
use super::errors::InjectionFileReadError;
pub const DEFAULT_FILE_NAMES: &[&str] = &["AGENTS.md"];
pub const DEFAULT_MAX_BYTES_PER_FILE: usize = 32 * 1024;
pub const DEFAULT_MAX_BYTES_PER_READ: usize = 128 * 1024;
#[derive(Clone, Debug)]
pub struct InjectedFileInfo { pub absolute_path: PathBuf, pub directory: PathBuf, pub truncated: bool, pub original_bytes: usize, pub injected_bytes: usize }
#[derive(Debug, Default)]
pub struct InjectionResult { pub injected_text: String, pub injected_files: Vec<InjectedFileInfo>, pub errors: Vec<InjectionFileReadError> }
pub struct InjectionConfig { pub file_names: Vec<String>, pub max_bytes_per_file: usize, pub max_bytes_per_read: usize }
impl Default for InjectionConfig {
 fn default() -> Self { Self { file_names: DEFAULT_FILE_NAMES.iter().map(|name| (*name).into()).collect(), max_bytes_per_file: DEFAULT_MAX_BYTES_PER_FILE, max_bytes_per_read: DEFAULT_MAX_BYTES_PER_READ } }
}
