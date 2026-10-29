use crate::path_posix::normalize_segments;

fn is_sep(character: char) -> bool {
    character == '\\' || character == '/'
}

struct Root<'a> {
    device: String,
    absolute: bool,
    rest: &'a str,
    root_end: usize,
}

fn parse_root(path: &str) -> Root<'_> {
    let characters: Vec<char> = path.chars().take(3).collect();
    let starts_with_sep = characters
        .first()
        .is_some_and(|character| is_sep(*character));
    if starts_with_sep
        && characters
            .get(1)
            .is_some_and(|character| is_sep(*character))
    {
        let body = &path[2..];
        let mut parts = body.splitn(3, is_sep);
        let server = parts.next().unwrap_or_default();
        let share = parts.next();
        if let (false, Some(share)) = (server.is_empty(), share.filter(|share| !share.is_empty())) {
            let consumed = 2 + server.len() + 1 + share.len();
            let rest = path.get(consumed..).unwrap_or_default();
            let root_end = if rest.is_empty() {
                consumed
            } else {
                consumed + 1
            };
            return Root {
                device: format!("\\\\{server}\\{share}"),
                absolute: true,
                rest,
                root_end,
            };
        }
        return Root {
            device: String::new(),
            absolute: true,
            rest: path,
            root_end: 1,
        };
    }
    if starts_with_sep {
        return Root {
            device: String::new(),
            absolute: true,
            rest: path,
            root_end: 1,
        };
    }
    if characters.len() >= 2 && characters[0].is_ascii_alphabetic() && characters[1] == ':' {
        let absolute = characters
            .get(2)
            .is_some_and(|character| is_sep(*character));
        return Root {
            device: path[..2].to_string(),
            absolute,
            rest: &path[2..],
            root_end: if absolute { 3 } else { 2 },
        };
    }
    Root {
        device: String::new(),
        absolute: false,
        rest: path,
        root_end: 0,
    }
}

pub fn is_absolute(path: &str) -> bool {
    let root = parse_root(path);
    root.absolute
}

pub fn normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let root = parse_root(path);
    let trailing = path.ends_with(is_sep);
    let mut tail = normalize_segments(root.rest, !root.absolute, is_sep).join("\\");
    if tail.is_empty() && !root.absolute {
        tail = ".".to_string();
    }
    if !tail.is_empty() && trailing {
        tail.push('\\');
    }
    let separator = if root.absolute { "\\" } else { "" };
    format!("{}{separator}{tail}", root.device)
}

pub fn join(paths: &[&str]) -> String {
    let joined = paths
        .iter()
        .filter(|path| !path.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\\");
    if joined.is_empty() {
        return ".".to_string();
    }
    normalize(&joined)
}

pub fn dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let root = parse_root(path);
    let bytes = path.as_bytes();
    let mut end: Option<usize> = None;
    let mut matched_slash = true;
    for index in (root.root_end..bytes.len()).rev() {
        if bytes[index] == b'\\' || bytes[index] == b'/' {
            if !matched_slash {
                end = Some(index);
                break;
            }
        } else {
            matched_slash = false;
        }
    }
    match end {
        Some(end) => path[..end].to_string(),
        None if root.root_end == 0 => ".".to_string(),
        None => path[..root.root_end].to_string(),
    }
}

pub fn basename(path: &str) -> String {
    let root = parse_root(path);
    let rest = if root.device.len() == 2 {
        &path[2..]
    } else {
        path
    };
    let trimmed = rest.trim_end_matches(is_sep);
    trimmed
        .rsplit(is_sep)
        .next()
        .unwrap_or_default()
        .to_string()
}

pub fn resolve(paths: &[&str]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for path in paths.iter().rev().filter(|path| !path.is_empty()) {
        parts.push((*path).to_string());
        if is_absolute(path) {
            break;
        }
    }
    if !parts.last().is_some_and(|path| is_absolute(path)) {
        parts.push(
            std::env::current_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "\\".to_string()),
        );
    }
    parts.reverse();
    let normalized = normalize(&parts.join("\\"));
    let root = parse_root(&normalized);
    if normalized.len() > root.root_end && normalized.ends_with('\\') {
        normalized[..normalized.len() - 1].to_string()
    } else {
        normalized
    }
}

pub fn relative(from: &str, to: &str) -> String {
    let from = resolve(&[from]);
    let to = resolve(&[to]);
    if from.to_lowercase() == to.to_lowercase() {
        return String::new();
    }
    let from_root = parse_root(&from);
    let to_root = parse_root(&to);
    if from_root.device.to_lowercase() != to_root.device.to_lowercase() {
        return to;
    }
    let from_segments: Vec<&str> = from_root
        .rest
        .split(is_sep)
        .filter(|segment| !segment.is_empty())
        .collect();
    let to_segments: Vec<&str> = to_root
        .rest
        .split(is_sep)
        .filter(|segment| !segment.is_empty())
        .collect();
    let common = from_segments
        .iter()
        .zip(&to_segments)
        .take_while(|(left, right)| left.to_lowercase() == right.to_lowercase())
        .count();
    let mut parts: Vec<&str> = vec![".."; from_segments.len() - common];
    parts.extend(&to_segments[common..]);
    parts.join("\\")
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::{basename, dirname, join, normalize, relative, resolve};

    #[test]
    fn win32_operations_match_node_semantics() {
        assert_eq!(
            normalize("C:\\USERS\\Alice\\profiles\\Kimi\\"),
            "C:\\USERS\\Alice\\profiles\\Kimi\\"
        );
        assert_eq!(
            normalize("C:\\Users\\Alice/.config/opencode"),
            "C:\\Users\\Alice\\.config\\opencode"
        );
        assert_eq!(
            join(&["C:\\Users\\Alice", "project", "..", "x"]),
            "C:\\Users\\Alice\\x"
        );
        assert_eq!(dirname("C:\\Users\\Alice"), "C:\\Users");
        assert_eq!(dirname("C:\\"), "C:\\");
        assert_eq!(basename("C:\\a\\Kimi\\"), "Kimi");
        assert_eq!(resolve(&["C:\\a\\b\\"]), "C:\\a\\b");
        assert_eq!(
            relative("C:\\Users\\Alice", "c:\\users\\alice\\Work\\x"),
            "Work\\x"
        );
        assert_eq!(relative("C:\\a", "D:\\b"), "D:\\b");
    }
}
