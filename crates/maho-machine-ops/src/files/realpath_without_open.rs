//! Port of `packages/machine-ops/src/files/realpath-without-open.ts`.

use crate::files::bounded_realpath::{is_missing_path_error, symlink_loop_error};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// Symlink hops allowed while resolving one path; realpath(3) reports ELOOP past this.
const MAX_SYMLINK_HOPS: usize = 40;

struct OpenFreeResolution {
    path: PathBuf,
    error: Option<std::io::Error>,
}

fn make_absolute(input: &Path) -> std::io::Result<PathBuf> {
    if input.is_absolute() {
        Ok(input.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(input))
    }
}

fn resolve_without_open(input: &Path) -> OpenFreeResolution {
    let absolute = match make_absolute(input) {
        Ok(path) => path,
        Err(error) => {
            return OpenFreeResolution {
                path: input.to_path_buf(),
                error: Some(error),
            };
        }
    };
    let mut pending: VecDeque<_> = absolute
        .components()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    let mut resolved = PathBuf::new();
    let mut hops = 0usize;
    while let Some(part) = pending.pop_front() {
        if part == "." {
            continue;
        }
        if part == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&part);
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.is_symlink() => {
                hops += 1;
                if hops > MAX_SYMLINK_HOPS {
                    let mut rest = candidate;
                    for p in pending {
                        rest.push(p);
                    }
                    return OpenFreeResolution {
                        path: rest,
                        error: Some(symlink_loop_error()),
                    };
                }
                let target = match std::fs::read_link(&candidate) {
                    Ok(t) => t,
                    Err(error) => {
                        let mut rest = candidate;
                        for p in pending {
                            rest.push(p);
                        }
                        return OpenFreeResolution {
                            path: rest,
                            error: Some(error),
                        };
                    }
                };
                if target.is_absolute() {
                    resolved.clear();
                }
                let parts: Vec<_> = target
                    .components()
                    .map(|c| c.as_os_str().to_owned())
                    .collect();
                for component in parts.into_iter().rev() {
                    pending.push_front(component);
                }
            }
            Ok(_) => resolved.push(part),
            Err(error) if is_missing_path_error(&error) => {
                resolved.push(part);
                for p in pending {
                    if p == ".." {
                        resolved.pop();
                    } else if p != "." {
                        resolved.push(p);
                    }
                }
                return OpenFreeResolution {
                    path: resolved,
                    error: Some(error),
                };
            }
            Err(error) => {
                let mut rest = candidate;
                for p in pending {
                    rest.push(p);
                }
                return OpenFreeResolution {
                    path: rest,
                    error: Some(error),
                };
            }
        }
    }
    OpenFreeResolution {
        path: resolved,
        error: None,
    }
}

/// Resolve symlinks without ever open(2)-ing a component, tolerating every error.
pub fn realpath_without_open(input: &Path) -> PathBuf {
    resolve_without_open(input).path
}

/// `realpath_without_open` that refuses to answer with a guess on non-missing errors.
pub fn realpath_without_open_strict(input: &Path) -> std::io::Result<PathBuf> {
    let resolution = resolve_without_open(input);
    if let Some(error) = resolution.error
        && !is_missing_path_error(&error)
    {
        return Err(error);
    }
    Ok(resolution.path)
}
