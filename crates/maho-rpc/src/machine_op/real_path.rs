//! Port of `packages/coding-agent/src/modes/rpc/machine-op/real-path.ts`.
//!
//! Where a process starts, judged where the path really leads; asynchronous because the engine's loop
//! must never block on a wedged mount.

use maho_machine_ops::files::bounded_realpath::with_resolution_deadline;
use maho_machine_ops::{MachineOpError, OpErrorCode};
use std::path::{Component, Path, PathBuf};

enum Lookup<T> {
    Ok(T),
    Missing,
    Timeout,
}

enum LinkKind {
    Link,
    Plain,
}

/// Lexical `path.resolve(base, raw)`: applies `.` and `..` without touching the filesystem.
pub fn resolve_lexical(base: &Path, raw: &Path) -> PathBuf {
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        base.join(raw)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

async fn bounded_canonicalize(path: PathBuf) -> Lookup<PathBuf> {
    let task = tokio::task::spawn_blocking(move || std::fs::canonicalize(&path));
    match with_resolution_deadline(task).await {
        Some(Ok(Ok(resolved))) => Lookup::Ok(resolved),
        Some(Ok(Err(_))) | Some(Err(_)) => Lookup::Missing,
        None => Lookup::Timeout,
    }
}

async fn bounded_link_kind(path: PathBuf) -> Lookup<LinkKind> {
    let task = tokio::task::spawn_blocking(move || {
        std::fs::symlink_metadata(&path).map(|info| {
            if info.file_type().is_symlink() {
                LinkKind::Link
            } else {
                LinkKind::Plain
            }
        })
    });
    match with_resolution_deadline(task).await {
        Some(Ok(Ok(kind))) => Lookup::Ok(kind),
        Some(Ok(Err(_))) | Some(Err(_)) => Lookup::Missing,
        None => Lookup::Timeout,
    }
}

fn is_drive_relative(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn split_leading_root(raw: &str) -> (Option<&str>, &str) {
    if Path::new(raw).is_absolute() {
        if let Some(rest) = raw.strip_prefix('/') {
            (Some("/"), rest)
        } else {
            (None, raw)
        }
    } else {
        (None, raw)
    }
}

async fn real_path_under(base: &str, raw: &str) -> Option<PathBuf> {
    let (abs_root, tail) = split_leading_root(raw);
    let start_root = PathBuf::from(abs_root.unwrap_or(base));
    let mut cur = match bounded_canonicalize(start_root).await {
        Lookup::Ok(path) => path,
        Lookup::Missing | Lookup::Timeout => return None,
    };
    let mut missing = false;
    for seg in tail.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        // A drive-relative segment (C:foo) names another drive's cwd on Windows; never a jailed name.
        if is_drive_relative(seg) {
            return None;
        }
        if seg == ".." {
            if let Some(parent) = cur.parent() {
                cur = parent.to_path_buf();
            }
            continue;
        }
        let next = cur.join(seg);
        if missing {
            cur = next;
            continue;
        }
        match bounded_link_kind(next.clone()).await {
            Lookup::Timeout => return None,
            Lookup::Missing => {
                missing = true;
                cur = next;
            }
            Lookup::Ok(LinkKind::Plain) => {
                cur = next;
            }
            Lookup::Ok(LinkKind::Link) => match bounded_canonicalize(next).await {
                Lookup::Ok(target) => cur = target,
                Lookup::Missing | Lookup::Timeout => return None,
            },
        }
    }
    Some(cur)
}

fn within(root: &Path, target: &Path) -> bool {
    target.starts_with(root)
}

/// The real place a process starts, checked against a confined root (returns `PolicyDenied`);
/// an unconfined root only anchors a relative cwd, and the OS decides whether the process may start there.
pub async fn spawn_path_in(
    root: &str,
    raw: &str,
    confined: bool,
) -> Result<PathBuf, MachineOpError> {
    let real = real_path_under(root, raw).await;
    if !confined {
        return Ok(real.unwrap_or_else(|| resolve_lexical(Path::new(root), Path::new(raw))));
    }
    let real_root = real_path_under(root, ".").await;
    match (real_root, real) {
        (Some(rr), Some(r)) if within(&rr, &r) => Ok(r),
        _ => Err(MachineOpError::new(
            OpErrorCode::PolicyDenied,
            format!("{raw} does not resolve inside {root}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn confines_paths_and_rejects_escapes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&root).expect("mkdir root");
        std::fs::create_dir_all(&outside).expect("mkdir outside");
        let root_str = root.to_string_lossy().into_owned();

        let inside = spawn_path_in(&root_str, "sub/dir", true)
            .await
            .expect("inside");
        assert!(inside.ends_with("sub/dir"));

        let escaped = spawn_path_in(&root_str, "../outside", true)
            .await
            .expect_err("must deny escape");
        assert_eq!(escaped.code, OpErrorCode::PolicyDenied);

        let unconfined = spawn_path_in(&root_str, "../outside", false)
            .await
            .expect("unconfined allows outside");
        assert!(unconfined.ends_with("outside"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlink_escaping_confined_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&root).expect("mkdir root");
        std::fs::create_dir_all(&outside).expect("mkdir outside");
        std::os::unix::fs::symlink(&outside, root.join("link")).expect("symlink");

        let root_str = root.to_string_lossy().into_owned();
        let err = spawn_path_in(&root_str, "link", true)
            .await
            .expect_err("symlink escape must deny");
        assert_eq!(err.code, OpErrorCode::PolicyDenied);
    }
}
