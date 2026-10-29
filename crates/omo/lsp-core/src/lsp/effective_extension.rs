use std::path::Path;

/// TS `effectiveExtension`: `Dockerfile`/`Containerfile` map to `.dockerfile`, otherwise
/// Node `extname` semantics (leading-dot names have no extension).
pub fn effective_extension(file_path: &str) -> String {
    let base = basename(file_path);
    match base {
        "Dockerfile" | "Containerfile" => ".dockerfile".to_string(),
        _ => extname(base),
    }
}

fn basename(file_path: &str) -> &str {
    let trimmed = file_path.trim_end_matches('/');
    Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
}

/// Node `path.extname` applied to a basename.
fn extname(base: &str) -> String {
    match base.rfind('.') {
        Some(0) | None => String::new(),
        Some(index) => {
            if base[..index].chars().all(|c| c == '.') {
                String::new()
            } else {
                base[index..].to_string()
            }
        }
    }
}
