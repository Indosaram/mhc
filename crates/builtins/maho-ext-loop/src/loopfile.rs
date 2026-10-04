use std::{fs::File, io::{self, Read}, path::{Path, PathBuf}, time::UNIX_EPOCH};
use sha2::{Digest, Sha256};
pub const MAX_MODEL_BYTES: usize = 25000;
pub const READ_LIMIT: usize = MAX_MODEL_BYTES + 1;
const TRUNCATION_WARNING: &str = "[loop.md truncated to the first 25000 bytes]";
#[derive(Clone, Debug, PartialEq)]
pub struct LoopFileFingerprint { pub path: PathBuf, pub mtime_ms: f64, pub size: u64, pub content_hash: String }
#[derive(Clone, Debug, PartialEq)]
pub struct LoopFileResult { pub path: PathBuf, pub content: String, pub fingerprint: LoopFileFingerprint }
#[derive(Debug, thiserror::Error)]
pub enum LoopFileError {
    #[error("Failed to stat loop file {path}")]
    StatFailed { path: PathBuf, #[source] cause: io::Error },
    #[error("Failed to read loop file {path}")]
    ReadFailed { path: PathBuf, #[source] cause: io::Error },
}
impl LoopFileError { pub const fn code(&self) -> &'static str { match self { Self::StatFailed { .. } => "stat_failed", Self::ReadFailed { .. } => "read_failed" } } }
pub struct FileStat { pub mtime_ms: f64, pub size: u64 }
pub trait LoopFileFs { fn stat(&self, path: &Path) -> io::Result<FileStat>; fn read_bytes(&self, path: &Path, max_bytes: usize) -> io::Result<Vec<u8>>; }
pub struct NodeFs;
impl LoopFileFs for NodeFs {
    fn stat(&self, path: &Path) -> io::Result<FileStat> {
        let metadata = std::fs::metadata(path)?;
        let modified = metadata.modified()?;
        let mtime_ms = match modified.duration_since(UNIX_EPOCH) { Ok(duration) => duration.as_secs_f64() * 1000.0, Err(error) => -error.duration().as_secs_f64() * 1000.0 };
        Ok(FileStat { mtime_ms, size: metadata.len() })
    }
    fn read_bytes(&self, path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> { let mut bytes = vec![0; max_bytes]; let read = File::open(path)?.read(&mut bytes)?; bytes.truncate(read); Ok(bytes) }
}
fn safe_utf8_truncate(bytes: &[u8], max_bytes: usize) -> &str {
    let mut slice = &bytes[..bytes.len().min(max_bytes)];
    loop { match std::str::from_utf8(slice) { Ok(text) => return text.strip_prefix('\u{feff}').unwrap_or(text), Err(_) if !slice.is_empty() => slice = &slice[..slice.len() - 1], Err(_) => return "" } }
}
pub fn resolve_loop_file_at(candidates: &[PathBuf], fs: &dyn LoopFileFs) -> Result<Option<LoopFileResult>, LoopFileError> {
    for path in candidates {
        let stat = match fs.stat(path) { Ok(stat) => stat, Err(error) if error.kind() == io::ErrorKind::NotFound => continue, Err(cause) => return Err(LoopFileError::StatFailed { path: path.clone(), cause }) };
        let raw = match fs.read_bytes(path, READ_LIMIT) { Ok(bytes) => bytes, Err(error) if error.kind() == io::ErrorKind::NotFound => continue, Err(cause) => return Err(LoopFileError::ReadFailed { path: path.clone(), cause }) };
        let content = if raw.len() > MAX_MODEL_BYTES { format!("{}\n{TRUNCATION_WARNING}", safe_utf8_truncate(&raw, MAX_MODEL_BYTES)) } else { String::from_utf8_lossy(&raw).into_owned() };
        let fingerprint = LoopFileFingerprint { path: path.clone(), mtime_ms: stat.mtime_ms, size: stat.size, content_hash: hex::encode(Sha256::digest(content.as_bytes())) };
        return Ok(Some(LoopFileResult { path: path.clone(), content, fingerprint }));
    }
    Ok(None)
}
pub fn resolve_loop_file(cwd: &str, home_dir: &str, fs: &dyn LoopFileFs) -> Result<Option<LoopFileResult>, LoopFileError> {
    resolve_loop_file_at(&[Path::new(cwd).join(maho_core::config::config_dir_name()).join("loop.md"), Path::new(&maho_core::config::resolve_agent_dir(cwd, home_dir, None)).join("loop.md")], fs)
}
#[cfg(test)] mod tests {
    use super::*; use std::collections::BTreeMap;
    struct FakeFs { files: BTreeMap<PathBuf, (Vec<u8>, f64)> }
    impl LoopFileFs for FakeFs { fn stat(&self, path: &Path) -> io::Result<FileStat> { let (bytes, mtime_ms) = self.files.get(path).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?; Ok(FileStat { size: u64::try_from(bytes.len()).unwrap(), mtime_ms: *mtime_ms }) } fn read_bytes(&self, path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> { let (bytes, _) = self.files.get(path).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?; Ok(bytes[..bytes.len().min(max_bytes)].to_vec()) } }
    fn fake(files: &[(&str, &str, f64)]) -> FakeFs { FakeFs { files: files.iter().map(|(p, s, t)| (PathBuf::from(p), (s.as_bytes().to_vec(), *t))).collect() } }
    fn candidates() -> [PathBuf; 2] { ["project".into(), "agent".into()] }
    #[test] fn project_precedes_agent() { let fs = fake(&[("project", "project tasks", 1000.0), ("agent", "agent tasks", 2000.0)]); let result = resolve_loop_file_at(&candidates(), &fs).unwrap().unwrap(); assert_eq!(result.path, PathBuf::from("project")); assert_eq!(result.fingerprint.size, 13); assert_eq!(result.fingerprint.content_hash, hex::encode(Sha256::digest(b"project tasks"))); }
    #[test] fn absent_project_falls_back() { let fs = fake(&[("agent", "agent tasks", 2000.0)]); let result = resolve_loop_file_at(&candidates(), &fs).unwrap().unwrap(); assert_eq!(result.path, PathBuf::from("agent")); }
    #[test] fn both_absent_returns_none() { let fs = fake(&[]); let result = resolve_loop_file_at(&candidates(), &fs).unwrap(); assert!(result.is_none()); }
    #[test] fn hashes_truncated_model_visible_text() { let content = "x".repeat(30000); let fs = fake(&[("project", &content, 1000.0)]); let result = resolve_loop_file_at(&candidates(), &fs).unwrap().unwrap(); assert_eq!(result.content, format!("{}\n{TRUNCATION_WARNING}", "x".repeat(25000))); assert_eq!(result.fingerprint.content_hash, hex::encode(Sha256::digest(result.content.as_bytes()))); assert_eq!(result.fingerprint.size, 30000); }
    #[test] fn multibyte_boundary_is_not_split() { let content = format!("{}中", "a".repeat(24999)); let fs = fake(&[("project", &content, 1000.0)]); let result = resolve_loop_file_at(&candidates(), &fs).unwrap().unwrap(); assert!(!result.content.contains('\u{fffd}')); assert_eq!(result.content.len(), 24999 + 1 + TRUNCATION_WARNING.len()); }
    #[test] fn mtime_changes_fingerprint_without_changing_hash() { let first = resolve_loop_file_at(&candidates(), &fake(&[("project", "tasks", 1000.0)])).unwrap().unwrap(); let second = resolve_loop_file_at(&candidates(), &fake(&[("project", "tasks", 2000.0)])).unwrap().unwrap(); assert_ne!(first.fingerprint, second.fingerprint); assert_eq!(first.fingerprint.content_hash, second.fingerprint.content_hash); }
    struct ErrorFs { read: bool }
    impl LoopFileFs for ErrorFs { fn stat(&self, _: &Path) -> io::Result<FileStat> { if self.read { Ok(FileStat { size: 5, mtime_ms: 0.0 }) } else { Err(io::ErrorKind::PermissionDenied.into()) } } fn read_bytes(&self, _: &Path, _: usize) -> io::Result<Vec<u8>> { Err(io::ErrorKind::PermissionDenied.into()) } }
    #[test] fn stat_error_is_typed() { let result = resolve_loop_file_at(&candidates(), &ErrorFs { read: false }).unwrap_err(); assert_eq!(result.code(), "stat_failed"); }
    #[test] fn read_error_is_typed() { let result = resolve_loop_file_at(&candidates(), &ErrorFs { read: true }).unwrap_err(); assert_eq!(result.code(), "read_failed"); }
    #[test] fn real_file_entry_point_reads_and_closes_file() { let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("loop.md"); std::fs::write(&path, b"tasks").unwrap(); let result = resolve_loop_file_at(std::slice::from_ref(&path), &NodeFs).unwrap().unwrap(); assert_eq!(result.content, "tasks"); std::fs::remove_file(path).unwrap(); }
}
