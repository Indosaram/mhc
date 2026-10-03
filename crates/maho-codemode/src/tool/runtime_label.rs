use super::types::EvalLanguage;

pub fn format_runtime_badge(language: EvalLanguage, name: &str, version: &str, path: Option<&str>, home: &str) -> String {
    let label = if language == EvalLanguage::Js { format!("{name} {version}") } else { version.into() };
    match path.filter(|path| !path.is_empty()) {
        Some(path) => format!("{label}, {}", minify_path(path, home)),
        None => label,
    }
}

pub fn minify_path(path: &str, home: &str) -> String {
    let contracted = if !home.is_empty() && path == home { "~".into() }
        else if !home.is_empty() && path.strip_prefix(home).is_some_and(|suffix| suffix.starts_with('/') || suffix.starts_with('\\')) { format!("~{}", &path[home.len()..]) }
        else { path.to_owned() };
    if contracted.chars().count() <= 40 { return contracted; }
    let separator = if contracted.contains('/') { '/' } else { '\\' };
    let segments: Vec<_> = contracted.split(separator).filter(|segment| !segment.is_empty()).collect();
    let first = segments.first().copied().unwrap_or("");
    let head = if contracted.starts_with(separator) { format!("{separator}{first}") } else { first.into() };
    let mut tail = Vec::new();
    for segment in segments.iter().skip(1).rev() {
        let mut attempt = vec![*segment];
        attempt.extend(tail.iter().copied());
        let joined = format!("{head}{separator}…{separator}{}", attempt.join(&separator.to_string()));
        if joined.chars().count() > 40 { break; }
        tail = attempt;
    }
    if !tail.is_empty() { return format!("{head}{separator}…{separator}{}", tail.join(&separator.to_string())); }
    let characters: Vec<_> = contracted.chars().collect();
    format!("…{}", characters[characters.len().saturating_sub(39)..].iter().collect::<String>())
}
