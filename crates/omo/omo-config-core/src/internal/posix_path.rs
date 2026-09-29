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

pub fn account_home_dir() -> String {
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
