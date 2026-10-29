pub fn strip_verbatim_prefix(path: &str) -> std::borrow::Cow<'_, str> {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        std::borrow::Cow::Owned(format!(r"\\{rest}"))
    } else if let Some(rest) = path.strip_prefix("//?/UNC/") {
        std::borrow::Cow::Owned(format!("//{rest}"))
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        std::borrow::Cow::Borrowed(rest)
    } else if let Some(rest) = path.strip_prefix("//?/") {
        std::borrow::Cow::Borrowed(rest)
    } else {
        std::borrow::Cow::Borrowed(path)
    }
}

pub fn to_posix_path(path: &str) -> String {
    let stripped = strip_verbatim_prefix(path);
    stripped.replace('\\', "/")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PathRoot<'a> {
    prefix: &'a str,
    is_absolute: bool,
    rest: &'a str,
}

fn parse_posix_root(path: &str) -> PathRoot<'_> {
    let bytes = path.as_bytes();
    // Drive letters and UNC shares are roots only on Windows; elsewhere `C:` and `//host` are
    // ordinary POSIX segments.
    if cfg!(windows) && bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        if bytes.len() >= 3 && (bytes[2] == b'/' || bytes[2] == b'\\') {
            return PathRoot {
                prefix: &path[..2],
                is_absolute: true,
                rest: path[3..].trim_start_matches(['/', '\\']),
            };
        } else {
            return PathRoot {
                prefix: &path[..2],
                is_absolute: false,
                rest: &path[2..],
            };
        }
    }
    if cfg!(windows)
        && (path.starts_with("//") || path.starts_with(r"\\"))
        && !path.starts_with("///")
        && !path.starts_with(r"\\\")
    {
        let after_slashes = &path[2..];
        if let Some(first_sep) = after_slashes.find(['/', '\\']) {
            let server = &after_slashes[..first_sep];
            let after_server = &after_slashes[first_sep + 1..];
            if !server.is_empty() {
                if let Some(second_sep) = after_server.find(['/', '\\']) {
                    let share = &after_server[..second_sep];
                    if !share.is_empty() {
                        let consumed = 2 + server.len() + 1 + share.len();
                        return PathRoot {
                            prefix: &path[..consumed],
                            is_absolute: true,
                            rest: path[consumed..].trim_start_matches(['/', '\\']),
                        };
                    }
                } else if !after_server.is_empty() {
                    return PathRoot {
                        prefix: path,
                        is_absolute: true,
                        rest: "",
                    };
                }
            }
        }
    }
    if path.starts_with('/') || path.starts_with('\\') {
        let rest = path.trim_start_matches(['/', '\\']);
        return PathRoot {
            prefix: "",
            is_absolute: true,
            rest,
        };
    }
    PathRoot {
        prefix: "",
        is_absolute: false,
        rest: path,
    }
}

fn normalize(path: &str) -> String {
    let posix = to_posix_path(path);
    if posix.is_empty() {
        return String::new();
    }
    let root = parse_posix_root(&posix);
    let mut segments: Vec<&str> = Vec::new();
    for segment in root.rest.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.last().is_some_and(|last| *last != "..") {
                    segments.pop();
                } else if !root.is_absolute {
                    segments.push("..");
                }
            }
            other => segments.push(other),
        }
    }
    let tail = segments.join("/");
    if !root.prefix.is_empty() {
        if root.is_absolute {
            if tail.is_empty() {
                format!("{}/", root.prefix)
            } else {
                format!("{}/{tail}", root.prefix)
            }
        } else if tail.is_empty() {
            root.prefix.to_string()
        } else {
            format!("{}/{tail}", root.prefix)
        }
    } else if root.is_absolute {
        format!("/{tail}")
    } else if tail.is_empty() {
        if path.is_empty() {
            String::new()
        } else {
            ".".to_string()
        }
    } else {
        tail
    }
}

pub fn posix_join(segments: &[&str]) -> String {
    let joined: Vec<&str> = segments
        .iter()
        .copied()
        .filter(|part| !part.is_empty())
        .collect();
    if joined.is_empty() {
        return ".".to_string();
    }
    let normalized = normalize(&joined.join("/"));
    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

pub fn posix_dirname(path: &str) -> String {
    let normalized = to_posix_path(path);
    if normalized.is_empty() {
        return ".".to_string();
    }
    let root = parse_posix_root(&normalized);
    let trimmed = normalized.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    if !root.prefix.is_empty() && trimmed == root.prefix {
        return if root.is_absolute {
            format!("{}/", root.prefix)
        } else {
            root.prefix.to_string()
        };
    }
    match trimmed.rfind('/') {
        None => {
            if root.is_absolute {
                if !root.prefix.is_empty() {
                    format!("{}/", root.prefix)
                } else {
                    "/".to_string()
                }
            } else {
                ".".to_string()
            }
        }
        Some(0) => "/".to_string(),
        Some(index) => {
            if !root.prefix.is_empty() && index == root.prefix.len() {
                format!("{}/", root.prefix)
            } else {
                trimmed[..index].to_string()
            }
        }
    }
}

pub fn posix_resolve(segments: &[&str]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for segment in segments.iter().rev().filter(|s| !s.is_empty()) {
        let posix_seg = to_posix_path(segment);
        let is_absolute = parse_posix_root(&posix_seg).is_absolute;
        parts.push(posix_seg);
        if is_absolute {
            break;
        }
    }
    if !parts.last().is_some_and(|p| parse_posix_root(p).is_absolute) {
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|path| path.to_str().map(to_posix_path))
            .unwrap_or_else(|| "/".to_string());
        parts.push(cwd);
    }
    parts.reverse();
    let joined = parts.join("/");
    normalize(&joined)
}

pub fn platform_join(segments: &[&str], windows: bool) -> String {
    let joined = posix_join(segments);
    if windows {
        to_posix_path(&joined).replace('/', "\\")
    } else {
        joined
    }
}

pub fn platform_dirname(path: &str, windows: bool) -> String {
    if windows {
        let normalized = to_posix_path(path);
        let trimmed = normalized.trim_end_matches('/');
        if trimmed.is_empty() {
            return "\\".to_string();
        }
        if let Some(index) = trimmed[1..].find('/') {
            let head = &trimmed[..index + 1];
            if head.is_empty() {
                return "\\".to_string();
            }
            let head = head.trim_end_matches('/');
            if head.chars().nth(1) == Some(':') {
                return head.to_string();
            }
            return head.to_string();
        }
        if trimmed.chars().nth(1) == Some(':') {
            return trimmed.to_string();
        }
        ".".to_string()
    } else {
        posix_dirname(path)
    }
}

#[cfg(unix)]
fn account_home_dir_unix() -> Option<String> {
    // Node.js `os.userInfo().homedir` uses `getpwuid_r(getuid())` on Unix.
    // Query the OS passwd entry for the calling process uid so the account home is
    // independent of any temporary $HOME override across Linux, macOS, and BSDs.
    let uid = unsafe { libc::getuid() };
    let mut buf_size = match unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) } {
        -1 => 1024,
        n if n <= 0 => 1024,
        n => n as usize,
    };
    if buf_size < 1024 {
        buf_size = 1024;
    }

    loop {
        let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let mut buf: Vec<std::ffi::c_char> = vec![0; buf_size];

        // SAFETY:
        // - `pwd` points to uninitialized storage of `libc::passwd`, which `getpwuid_r` writes into.
        // - `buf` is allocated with `buf_size` capacity and passed with matching `buflen`.
        // - `result` is set to point to `pwd` on success, or null on error / user not found.
        // - When `res == 0` and `result != null`, `pwd` is fully initialized and valid.
        // - Strings referenced by `pwd` (such as `pw_dir`) reside within `buf`. `buf` remains
        //   alive in this scope while `pw_dir` is converted into an owned Rust String before returning.
        let (res, pw_dir_str) = unsafe {
            let ret = libc::getpwuid_r(
                uid,
                pwd.as_mut_ptr(),
                buf.as_mut_ptr(),
                buf_size,
                &mut result,
            );
            if ret == 0 && !result.is_null() {
                let pw = pwd.assume_init();
                let dir_opt = if pw.pw_dir.is_null() {
                    None
                } else {
                    std::ffi::CStr::from_ptr(pw.pw_dir).to_str().ok().map(str::to_string)
                };
                (ret, dir_opt)
            } else {
                (ret, None)
            }
        };

        if res == libc::ERANGE {
            buf_size = buf_size.checked_mul(2)?;
            continue;
        }
        if res != 0 {
            return None;
        }

        let dir = pw_dir_str?;
        if dir.is_empty() {
            return None;
        }
        return Some(to_posix_path(&dir));
    }
}

pub fn account_home_dir() -> String {
    #[cfg(unix)]
    if let Some(home) = account_home_dir_unix() {
        return home;
    }

    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|home| to_posix_path(&home))
        .unwrap_or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|path| path.to_str().map(to_posix_path))
                .unwrap_or_default()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_posix_path_operations() {
        assert_eq!(to_posix_path(r"a\b\c"), "a/b/c");
        assert_eq!(to_posix_path(r"\\?\C:\foo\bar"), "C:/foo/bar");
        assert_eq!(to_posix_path("//?/C:/foo/bar"), "C:/foo/bar");
        assert_eq!(to_posix_path(r"\\?\UNC\server\share\file"), "//server/share/file");

        assert_eq!(normalize("/home/alice/../bob"), "/home/bob");
        assert_eq!(normalize("/.."), "/");
        assert_eq!(normalize("."), ".");
        assert_eq!(normalize(""), "");

        assert_eq!(posix_join(&["/home/alice", ".omo"]), "/home/alice/.omo");

        assert_eq!(posix_dirname("/home/user/.omo/omo.jsonc"), "/home/user/.omo");
        assert_eq!(posix_dirname("/home"), "/");
        assert_eq!(posix_dirname("/"), "/");
        assert_eq!(posix_dirname("file.txt"), ".");

        assert_eq!(posix_resolve(&["/home/alice"]), "/home/alice");
        assert_eq!(posix_resolve(&["/base", "sub"]), "/base/sub");
    }

    #[cfg(not(windows))]
    #[test]
    fn drive_letters_and_double_slashes_are_plain_segments_off_windows() {
        assert_eq!(normalize("C:/Users/alice/../bob"), "C:/Users/bob");
        assert_eq!(normalize("//host/share/x"), "/host/share/x");
    }

    #[cfg(windows)]
    #[test]
    fn drive_letters_and_unc_shares_are_roots_on_windows() {
        assert_eq!(normalize("C:/Users/alice/../bob"), "C:/Users/bob");
        assert_eq!(normalize("C:/.."), "C:/");
        assert_eq!(posix_join(&["C:/Users/alice", ".omo", "omo.jsonc"]), "C:/Users/alice/.omo/omo.jsonc");
        assert_eq!(posix_dirname("C:/Users/me/.omo/omo.jsonc"), "C:/Users/me/.omo");
        assert_eq!(posix_dirname("C:/Users/me"), "C:/Users");
        assert_eq!(posix_dirname("C:/Users"), "C:/");
        assert_eq!(posix_dirname("C:/"), "C:/");
        assert_eq!(posix_resolve(&["C:/Users/alice"]), "C:/Users/alice");
        assert_eq!(posix_resolve(&[r"C:\Users\alice"]), "C:/Users/alice");
        assert_eq!(posix_resolve(&["C:/base", "sub"]), "C:/base/sub");
    }
}
