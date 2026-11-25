//! Port of `packages/machine-ops/src/files/local-files.ts`.

use crate::errors::{MachineOpError, OpErrorCode};
use crate::files::glob::{FdResolver, local_glob, walk_glob};
use crate::files::grep::local_grep;
use crate::files::mutation_queue::acquire_file_mutation_lock;
use crate::policy::filesystem::FilesystemOperation;
use crate::policy::gate::{check_path, gated, normalize_resolve, resolve_op_path};
use crate::types::{
    EditRequest, EditResult, EntryType, FileOps, GlobRequest, GlobResult, GrepRequest, GrepResult,
    ListEntry, ListRequest, ListResult, OpContext, OpFuture, OpName, PathLockMode, ReadRequest,
    ReadResult, StatAccess, StatRequest, StatResult, WriteRequest, WriteResult,
};
use serde_json::json;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

pub struct LocalFileOps {
    resolve_fd: Option<FdResolver>,
    force_walk: bool,
}

impl LocalFileOps {
    pub fn new(resolve_fd: Option<FdResolver>) -> Self {
        Self {
            resolve_fd,
            force_walk: false,
        }
    }

    /// Use the built-in walking backend without consulting `fd` on `PATH` (for deterministic tests).
    pub fn new_walking() -> Self {
        Self {
            resolve_fd: None,
            force_walk: true,
        }
    }
}

fn entry_type(ft: &std::fs::FileType) -> EntryType {
    if ft.is_file() {
        EntryType::File
    } else if ft.is_dir() {
        EntryType::Directory
    } else if ft.is_symlink() {
        EntryType::Symlink
    } else {
        EntryType::Other
    }
}

fn read_limit_error(path: &Path, size: u64, limit: usize) -> MachineOpError {
    MachineOpError::new(
        OpErrorCode::LimitExceeded,
        format!(
            "{} is {size} bytes, above the {limit}-byte read limit",
            path.display()
        ),
    )
}

async fn with_path_lock<T, F, Fut>(
    path: &Path,
    ctx: &OpContext,
    fn_call: F,
) -> Result<T, MachineOpError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, MachineOpError>>,
{
    if matches!(ctx.path_lock, Some(PathLockMode::Caller)) {
        return fn_call().await;
    }
    if ctx.signal.is_aborted() {
        return Err(MachineOpError::new(
            OpErrorCode::Aborted,
            "Operation aborted",
        ));
    }
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(None);
    let abort_sig = ctx.signal.clone();
    let forwarder = tokio::spawn(async move {
        abort_sig.cancelled().await;
        let _ = cancel_tx.send(Some(
            abort_sig
                .reason()
                .unwrap_or_else(|| "file mutation lock wait cancelled".to_owned()),
        ));
    });
    let release = acquire_file_mutation_lock(path, Some(cancel_rx)).await;
    forwarder.abort();
    let release = release.map_err(|err| {
        let code = if ctx.signal.is_aborted() {
            OpErrorCode::Aborted
        } else {
            OpErrorCode::IoError
        };
        MachineOpError::new(code, err.to_string())
    })?;
    let outcome = fn_call().await;
    release.release();
    outcome
}

async fn read_within_limit(path: &Path, limit: Option<usize>) -> Result<Vec<u8>, MachineOpError> {
    if let Some(limit) = limit {
        let info = tokio::fs::metadata(path)
            .await
            .map_err(|err| MachineOpError::from_io(&err, Some("stat"), Some(path)))?;
        if info.len() > limit as u64 {
            return Err(read_limit_error(path, info.len(), limit));
        }
    }
    tokio::fs::read(path)
        .await
        .map_err(|err| MachineOpError::from_io(&err, Some("open"), Some(path)))
}

async fn read_range(
    path: &Path,
    req: &ReadRequest,
    limit: Option<usize>,
) -> Result<ReadResult, MachineOpError> {
    let mut handle = tokio::fs::File::open(path)
        .await
        .map_err(|err| MachineOpError::from_io(&err, Some("open"), Some(path)))?;
    let info = handle
        .metadata()
        .await
        .map_err(|err| MachineOpError::from_io(&err, Some("fstat"), Some(path)))?;
    let offset = req.offset.unwrap_or(0);
    let available = info.len().saturating_sub(offset);
    if req.limit_bytes.is_none()
        && let Some(limit) = limit
        && available > limit as u64
    {
        return Err(read_limit_error(path, available, limit));
    }
    let length = match req.limit_bytes {
        Some(req_limit) => (available.min(req_limit as u64)) as usize,
        None => usize::try_from(available).unwrap_or(usize::MAX),
    };
    if offset > 0 {
        handle
            .seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|err| MachineOpError::from_io(&err, Some("lseek"), Some(path)))?;
    }
    let mut bytes = vec![0u8; length];
    let mut total_read = 0usize;
    while total_read < length {
        let n = handle
            .read(&mut bytes[total_read..])
            .await
            .map_err(|err| MachineOpError::from_io(&err, Some("read"), Some(path)))?;
        if n == 0 {
            break;
        }
        total_read += n;
    }
    bytes.truncate(total_read);
    Ok(ReadResult {
        path: path.to_path_buf(),
        bytes,
        size: info.len(),
        offset,
    })
}

async fn write_range(path: &Path, req: &WriteRequest) -> Result<(), MachineOpError> {
    let offset = req.offset.unwrap_or(0);
    if let Some(total) = req.total
        && total < offset.saturating_add(req.bytes.len() as u64)
    {
        return Err(MachineOpError::new(
            OpErrorCode::InvalidArgument,
            "total must cover offset + bytes",
        ));
    }
    let mut handle = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .await
        .map_err(|err| MachineOpError::from_io(&err, Some("open"), Some(path)))?;
    if offset > 0 {
        handle
            .seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|err| MachineOpError::from_io(&err, Some("lseek"), Some(path)))?;
    }
    handle
        .write_all(&req.bytes)
        .await
        .map_err(|err| MachineOpError::from_io(&err, Some("write"), Some(path)))?;
    if let Some(total) = req.total {
        handle
            .set_len(total)
            .await
            .map_err(|err| MachineOpError::from_io(&err, Some("ftruncate"), Some(path)))?;
    }
    handle
        .flush()
        .await
        .map_err(|err| MachineOpError::from_io(&err, Some("flush"), Some(path)))?;
    Ok(())
}

fn check_access(path: &Path, access_mode: StatAccess) -> Result<(), MachineOpError> {
    #[cfg(unix)]
    {
        use nix::unistd::{AccessFlags, access};
        let flags = match access_mode {
            StatAccess::Read => AccessFlags::R_OK,
            StatAccess::Write => AccessFlags::W_OK,
            StatAccess::ReadWrite => AccessFlags::R_OK | AccessFlags::W_OK,
        };
        access(path, flags).map_err(|errno| {
            let io_err = std::io::Error::from_raw_os_error(errno as i32);
            MachineOpError::from_io(&io_err, Some("access"), Some(path))
        })?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let meta = std::fs::metadata(path)
            .map_err(|err| MachineOpError::from_io(&err, Some("access"), Some(path)))?;
        if matches!(access_mode, StatAccess::Write | StatAccess::ReadWrite)
            && meta.permissions().readonly()
        {
            return Err(MachineOpError::with_fs_code(
                OpErrorCode::PermissionDenied,
                format!("EACCES: permission denied, access '{}'", path.display()),
                "EACCES",
            ));
        }
        Ok(())
    }
}

fn file_mode(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mode()
    }
    #[cfg(not(unix))]
    {
        if meta.permissions().readonly() {
            0o444
        } else {
            0o666
        }
    }
}

fn mtime_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn has_glob_syntax(segment: &str) -> bool {
    segment
        .chars()
        .any(|c| matches!(c, '*' | '?' | '[' | ']' | '{' | '}'))
}

fn is_pattern_absolute(pattern: &str) -> bool {
    Path::new(pattern).is_absolute() || pattern.starts_with('/') || pattern.starts_with('\\')
}

/// Split a glob pattern into the directory its fixed leading segments name (resolved against `cwd`) and the rest.
/// Returns `None` when `".."` follows a wildcard.
fn split_glob_base(pattern: &str, cwd: &Path) -> Option<(PathBuf, String)> {
    let normalized = pattern.replace('\\', "/");
    let segments: Vec<&str> = normalized.split('/').collect();
    let first_glob = segments.iter().position(|seg| has_glob_syntax(seg));
    let cut = first_glob.unwrap_or_else(|| segments.len().saturating_sub(1));
    let fixed = &segments[..cut];
    let rest = &segments[cut..];
    if rest.contains(&"..") {
        return None;
    }

    let mut fixed_rel = PathBuf::new();
    let pattern_abs = is_pattern_absolute(pattern);
    if pattern_abs {
        #[cfg(unix)]
        fixed_rel.push(std::path::MAIN_SEPARATOR_STR);
        #[cfg(windows)]
        {
            if let Some(prefix) = Path::new(pattern).components().next()
                && matches!(
                    prefix,
                    std::path::Component::Prefix(_) | std::path::Component::RootDir
                )
            {
                fixed_rel.push(prefix.as_os_str());
            }
        }
    }
    for seg in fixed {
        if seg.is_empty() && pattern_abs && fixed_rel.as_os_str().is_empty() {
            continue;
        }
        if !seg.is_empty() {
            fixed_rel.push(seg);
        }
    }
    let base = if pattern_abs {
        normalize_resolve(Path::new(std::path::MAIN_SEPARATOR_STR), &fixed_rel)
    } else {
        normalize_resolve(cwd, &fixed_rel)
    };
    let rest_joined = rest.join("/");
    if rest_joined.is_empty() {
        Some((base, "*".to_owned()))
    } else {
        Some((base, rest_joined))
    }
}

fn pattern_has_parent_segment(pattern: &str) -> bool {
    pattern.split(['/', '\\']).any(|seg| seg == "..")
}

impl FileOps for LocalFileOps {
    fn read<'a>(&'a self, req: ReadRequest, ctx: &'a OpContext) -> OpFuture<'a, ReadResult> {
        Box::pin(async move {
            let path = resolve_op_path(&req.path, ctx)?;
            let limit = ctx.limits.read_max_bytes;
            if let (Some(req_limit), Some(max_limit)) = (req.limit_bytes, limit)
                && req_limit > max_limit
            {
                return Err(MachineOpError::new(
                    OpErrorCode::LimitExceeded,
                    format!("limitBytes {req_limit} is above the {max_limit}-byte read limit"),
                ));
            }
            check_path(&path, FilesystemOperation::Read, OpName::FilesRead, ctx).await?;
            let args = json!({
                "path": req.path,
                "offset": req.offset,
                "limitBytes": req.limit_bytes,
            });
            gated(OpName::FilesRead, &args, ctx, || async {
                if req.offset.is_some() || req.limit_bytes.is_some() {
                    return read_range(&path, &req, limit).await;
                }
                let bytes = read_within_limit(&path, limit).await?;
                let size = bytes.len() as u64;
                Ok(ReadResult {
                    path,
                    bytes,
                    size,
                    offset: 0,
                })
            })
            .await
        })
    }

    fn write<'a>(&'a self, req: WriteRequest, ctx: &'a OpContext) -> OpFuture<'a, WriteResult> {
        Box::pin(async move {
            let path = resolve_op_path(&req.path, ctx)?;
            check_path(&path, FilesystemOperation::Write, OpName::FilesWrite, ctx).await?;
            let args = json!({ "path": req.path });
            let path_for_write = path.clone();
            with_path_lock(&path, ctx, || {
                gated(OpName::FilesWrite, &args, ctx, || async move {
                    if req.create_parent_dirs
                        && let Some(parent) = path_for_write.parent()
                    {
                        tokio::fs::create_dir_all(parent).await.map_err(|err| {
                            MachineOpError::from_io(&err, Some("mkdir"), Some(parent))
                        })?;
                    }
                    if req.offset.is_none() {
                        tokio::fs::write(&path_for_write, &req.bytes)
                            .await
                            .map_err(|err| {
                                MachineOpError::from_io(&err, Some("open"), Some(&path_for_write))
                            })?;
                    } else {
                        write_range(&path_for_write, &req).await?;
                    }
                    Ok(WriteResult {
                        path: path_for_write,
                        written_bytes: req.bytes.len(),
                    })
                })
            })
            .await
        })
    }

    fn edit<'a>(&'a self, req: EditRequest, ctx: &'a OpContext) -> OpFuture<'a, EditResult> {
        Box::pin(async move {
            let path = resolve_op_path(&req.path, ctx)?;
            if req.exact.is_empty() {
                return Err(MachineOpError::new(
                    OpErrorCode::InvalidPattern,
                    "exact substring must not be empty",
                ));
            }
            check_path(&path, FilesystemOperation::Write, OpName::FilesEdit, ctx).await?;
            let args = json!({ "path": req.path });
            let path_for_edit = path.clone();
            with_path_lock(&path, ctx, || {
                gated(OpName::FilesEdit, &args, ctx, || async move {
                    let raw = read_within_limit(&path_for_edit, ctx.limits.read_max_bytes).await?;
                    let content = String::from_utf8_lossy(&raw);
                    let pieces: Vec<&str> = content.split(&req.exact).collect();
                    if pieces.len() == 1 {
                        return Err(MachineOpError::new(
                            OpErrorCode::NotFound,
                            "exact substring was not found",
                        ));
                    }
                    let replacements = pieces.len() - 1;
                    let next = pieces.join(&req.replacement);
                    let bytes = next.as_bytes();
                    tokio::fs::write(&path_for_edit, bytes)
                        .await
                        .map_err(|err| {
                            MachineOpError::from_io(&err, Some("open"), Some(&path_for_edit))
                        })?;
                    Ok(EditResult {
                        path: path_for_edit.clone(),
                        content: next.clone(),
                        replacements,
                        write_result: WriteResult {
                            path: path_for_edit,
                            written_bytes: bytes.len(),
                        },
                    })
                })
            })
            .await
        })
    }

    fn glob<'a>(&'a self, req: GlobRequest, ctx: &'a OpContext) -> OpFuture<'a, GlobResult> {
        Box::pin(async move {
            let mut cwd = resolve_op_path(req.cwd.as_deref().unwrap_or("."), ctx)?;
            let mut pattern = req.pattern.clone();
            if ctx.scope.root.is_some()
                && (is_pattern_absolute(&pattern) || pattern_has_parent_segment(&pattern))
            {
                let Some((base, rest)) = split_glob_base(&pattern, &cwd) else {
                    return Err(MachineOpError::new(
                        OpErrorCode::PolicyDenied,
                        "a glob pattern may not use '..' after a wildcard",
                    ));
                };
                cwd = base;
                pattern = rest;
            }
            check_path(&cwd, FilesystemOperation::Enumerate, OpName::FilesGlob, ctx).await?;
            let args = json!({ "pattern": req.pattern, "cwd": req.cwd });
            gated(OpName::FilesGlob, &args, ctx, || async {
                if self.force_walk {
                    return walk_glob(&pattern, &cwd, req.limit.unwrap_or(1000), &ctx.signal).await;
                }
                local_glob(
                    &pattern,
                    &cwd,
                    req.limit,
                    &ctx.signal,
                    self.resolve_fd.as_ref(),
                )
                .await
            })
            .await
        })
    }

    fn grep<'a>(&'a self, req: GrepRequest, ctx: &'a OpContext) -> OpFuture<'a, GrepResult> {
        Box::pin(async move {
            let path = resolve_op_path(&req.path, ctx)?;
            check_path(
                &path,
                FilesystemOperation::Enumerate,
                OpName::FilesGrep,
                ctx,
            )
            .await?;
            let args = json!({ "path": req.path, "pattern": req.pattern });
            gated(OpName::FilesGrep, &args, ctx, || {
                local_grep(
                    &path,
                    &req.pattern,
                    req.limit,
                    ctx.limits.read_max_bytes,
                    &ctx.signal,
                )
            })
            .await
        })
    }

    fn stat<'a>(&'a self, req: StatRequest, ctx: &'a OpContext) -> OpFuture<'a, StatResult> {
        Box::pin(async move {
            let path = resolve_op_path(&req.path, ctx)?;
            check_path(&path, FilesystemOperation::Read, OpName::FilesStat, ctx).await?;
            let args = json!({ "path": req.path });
            gated(OpName::FilesStat, &args, ctx, || async {
                if let Some(access_mode) = req.access {
                    check_access(&path, access_mode)?;
                }
                let meta = tokio::fs::metadata(&path)
                    .await
                    .map_err(|err| MachineOpError::from_io(&err, Some("stat"), Some(&path)))?;
                Ok(StatResult {
                    path,
                    entry_type: entry_type(&meta.file_type()),
                    size: meta.len(),
                    mtime_ms: mtime_ms(&meta),
                    mode: file_mode(&meta),
                })
            })
            .await
        })
    }

    fn list<'a>(&'a self, req: ListRequest, ctx: &'a OpContext) -> OpFuture<'a, ListResult> {
        Box::pin(async move {
            let path = resolve_op_path(&req.path, ctx)?;
            check_path(
                &path,
                FilesystemOperation::Enumerate,
                OpName::FilesList,
                ctx,
            )
            .await?;
            let args = json!({ "path": req.path });
            gated(OpName::FilesList, &args, ctx, || async {
                let mut rd = tokio::fs::read_dir(&path)
                    .await
                    .map_err(|err| MachineOpError::from_io(&err, Some("scandir"), Some(&path)))?;
                let mut entries = Vec::new();
                while let Some(dirent) = rd
                    .next_entry()
                    .await
                    .map_err(|err| MachineOpError::from_io(&err, Some("scandir"), Some(&path)))?
                {
                    let ft = dirent.file_type().await.map_err(|err| {
                        MachineOpError::from_io(&err, Some("lstat"), Some(&dirent.path()))
                    })?;
                    entries.push(ListEntry {
                        name: dirent.file_name().to_string_lossy().into_owned(),
                        entry_type: entry_type(&ft),
                    });
                }
                Ok(ListResult { path, entries })
            })
            .await
        })
    }
}
