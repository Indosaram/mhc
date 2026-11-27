//! Reduce a memory child's stderr failure tail to one readable cause line (`worker/failure-detail.ts`).

pub const FAILURE_CAUSE_MAX_CHARS: usize = 200;
pub const FAILURE_FINGERPRINT_DETAIL_CHARS: usize = 60;

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{001b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn is_machinery_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Code frame row: `<digits> | ...`
    let digit_len = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digit_len > 0 && trimmed[digit_len..].trim_start().starts_with('|') {
        return true;
    }
    if trimmed.chars().all(|ch| ch == '^') {
        return true;
    }
    if let Some(after_at) = trimmed.strip_prefix("at")
        && after_at.starts_with(char::is_whitespace)
        && !after_at.trim().is_empty()
    {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    for prefix in [
        "path",
        "syscall",
        "errno",
        "code",
        "dest",
        "address",
        "port",
        "erroredsyscall",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix)
            && rest.trim_start().starts_with(':')
        {
            return true;
        }
    }
    if let Some(rest) = trimmed.strip_prefix("Bun v")
        && rest.chars().next().is_some_and(|ch| ch.is_ascii_digit())
    {
        return true;
    }
    if let Some(inner) = trimmed.strip_prefix("[truncated to last ")
        && let Some(bytes_part) = inner.strip_suffix(" bytes]")
        && !bytes_part.is_empty()
        && bytes_part.bytes().all(|b| b.is_ascii_digit())
    {
        return true;
    }
    false
}

fn is_cause_shaped(line: &str) -> bool {
    let word: String = line
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if word.is_empty() {
        return false;
    }
    let lower = word.to_ascii_lowercase();
    if matches!(lower.as_str(), "error" | "warning" | "fatal") {
        return true;
    }
    if word.len() >= 3 && word.starts_with('E') && word.bytes().all(|b| b.is_ascii_uppercase()) {
        return true;
    }
    word.starts_with(|ch: char| ch.is_ascii_uppercase())
        && (word.ends_with("Error") || word.ends_with("Exception"))
}

pub fn child_failure_cause(detail: Option<&str>) -> Option<String> {
    let detail = detail?;
    let lines: Vec<String> = detail
        .split('\n')
        .map(|line| {
            strip_ansi(line)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|line| !line.is_empty() && !is_machinery_line(line))
        .collect();
    let cause = lines
        .iter()
        .find(|line| is_cause_shaped(line))
        .or_else(|| lines.first())?;
    let count = cause.chars().count();
    if count <= FAILURE_CAUSE_MAX_CHARS {
        Some(cause.clone())
    } else {
        let clipped: String = cause.chars().take(FAILURE_CAUSE_MAX_CHARS - 1).collect();
        Some(format!("{clipped}…"))
    }
}

pub fn failure_fingerprint(reason: Option<&str>, detail: Option<&str>) -> String {
    let clipped: String = child_failure_cause(detail)
        .unwrap_or_default()
        .chars()
        .take(FAILURE_FINGERPRINT_DETAIL_CHARS)
        .collect();
    format!("{}:{clipped}", reason.unwrap_or("failed"))
}
