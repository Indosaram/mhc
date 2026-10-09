pub fn to_posix_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn split_segments(path: &str) -> (bool, Vec<String>) {
    let mut segments: Vec<String> = Vec::new();
    let mut absolute = false;
    let mut index = 0usize;
    let bytes: Vec<char> = path.chars().collect();
    while index < bytes.len() {
        while index < bytes.len() && bytes[index] == '/' {
            index += 1;
        }
        if index == 0 {
            continue;
        }
        if index == 1 && bytes[0] == '/' {
            absolute = true;
        }
        let start = index;
        while index < bytes.len() && bytes[index] != '/' {
            index += 1;
        }
        if start != index {
            segments.push(bytes[start..index].iter().collect());
        }
    }
    if path.starts_with('/') || path.starts_with('\\') {
        absolute = true;
    }
    (absolute, segments)
}

fn normalize(path: &str) -> String {
    let (absolute, segments) = split_segments(&to_posix_path(path));
    let mut out: Vec<String> = Vec::new();
    for segment in segments {
        match segment.as_str() {
            "." => {}
            ".." => {
                if let Some(last) = out.last()
                    && last != ".."
                {
                    out.pop();
                    continue;
                }
                if !absolute {
                    out.push(segment);
                }
            }
            _ => out.push(segment),
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        String::new()
    } else {
        joined
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
    let trimmed = normalized.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    match trimmed.rfind('/') {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(index) => trimmed[..index].to_string(),
    }
}

pub fn posix_resolve(segments: &[&str]) -> String {
    if segments.iter().any(|segment| segment.starts_with('/')) {
        let joined = segments
            .iter()
            .filter(|segment| !segment.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("/");
        return normalize(&joined);
    }
    let mut parts: Vec<&str> = Vec::new();
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(str::to_string))
        .unwrap_or_else(|| "/".to_string());
    parts.push(cwd.as_str());
    parts.extend(
        segments
            .iter()
            .filter(|segment| !segment.is_empty())
            .copied(),
    );
    normalize(&parts.join("/"))
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
