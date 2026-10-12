use base64::{Engine, engine::general_purpose::STANDARD};
use std::cmp::Ordering;

fn collation_key(character: char) -> (u8, u32) {
    if character.is_whitespace() || character.is_ascii_punctuation() {
        (0, character as u32)
    } else if character.is_ascii_digit() {
        (1, character as u32)
    } else if character.is_alphabetic() {
        let folded = character.to_lowercase().next().unwrap_or(character) as u32;
        (2, folded)
    } else {
        (3, character as u32)
    }
}

pub fn locale_compare(left: &str, right: &str) -> Ordering {
    let primary = left.chars().map(collation_key).cmp(right.chars().map(collation_key));
    if primary != Ordering::Equal {
        return primary;
    }
    left.chars().map(|character| u8::from(character.is_uppercase())).cmp(right.chars().map(|character| u8::from(character.is_uppercase())))
}

pub fn to_locale_lowercase(value: &str) -> String {
    value.to_lowercase()
}

pub fn date_parse_ms(raw: &str) -> Option<i64> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(parsed.timestamp_millis());
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc2822(value) {
        return Some(parsed.timestamp_millis());
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%dT%H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f%z"] {
        if let Ok(parsed) = chrono::DateTime::parse_from_str(value, format) {
            return Some(parsed.timestamp_millis());
        }
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M%.f",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(value, format) {
            return local_date_millis(parsed);
        }
    }
    if let Ok(parsed) = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Some(parsed.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis());
    }
    if value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return chrono::NaiveDate::from_ymd_opt(value.parse().ok()?, 1, 1)
            .and_then(|date| date.and_hms_opt(0, 0, 0)).map(|date| date.and_utc().timestamp_millis());
    }
    if value.len() == 7 && value.as_bytes()[4] == b'-' {
        return chrono::NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d").ok()
            .and_then(|date| date.and_hms_opt(0, 0, 0)).map(|date| date.and_utc().timestamp_millis());
    }
    for format in ["%m/%d/%Y %H:%M:%S%.f", "%m/%d/%Y %H:%M", "%Y/%m/%d %H:%M:%S%.f", "%Y/%m/%d %H:%M", "%b %e, %Y %H:%M:%S%.f", "%b %e, %Y %H:%M", "%b %e %Y %H:%M:%S%.f", "%b %e %Y %H:%M"] {
        if let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(value, format) { return local_date_millis(parsed); }
    }
    for format in ["%m/%d/%Y", "%Y/%m/%d", "%b %e, %Y", "%b %e %Y"] {
        if let Ok(parsed) = chrono::NaiveDate::parse_from_str(value, format) { return parsed.and_hms_opt(0, 0, 0).and_then(local_date_millis); }
    }
    None
}

fn local_date_millis(value: chrono::NaiveDateTime) -> Option<i64> {
    use chrono::TimeZone;
    match chrono::Local.from_local_datetime(&value) {
        chrono::LocalResult::Single(date) => Some(date.timestamp_millis()),
        chrono::LocalResult::Ambiguous(date, _) => Some(date.timestamp_millis()),
        chrono::LocalResult::None => chrono::Local.from_local_datetime(&(value - chrono::Duration::hours(1)))
            .earliest().map(|date| date.timestamp_millis() + 3_600_000),
    }
}

fn parse_int_prefix(text: &str) -> f64 {
    let trimmed = text.trim_start_matches(|character: char| character.is_ascii_whitespace());
    let (sign, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let digits = digits.chars().take_while(char::is_ascii_digit).collect::<String>();
    if digits.is_empty() {
        return f64::NAN;
    }
    sign * digits.parse::<f64>().unwrap_or(f64::NAN)
}

pub fn encode_cursor_number(offset: f64) -> String {
    STANDARD.encode(maho_ai::utils::js::number_to_string(offset))
}

pub fn decode_cursor_number(cursor: Option<&str>) -> f64 {
    let Some(cursor) = cursor else { return 0.0 };
    let normalized = cursor
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '+' | '/' | '-' | '_'))
        .map(|character| match character {
            '-' => '+',
            '_' => '/',
            other => other,
        })
        .collect::<String>();
    let mut normalized = normalized;
    while normalized.len() % 4 != 0 {
        normalized.push('=');
    }
    let Ok(decoded) = STANDARD.decode(normalized) else { return 0.0 };
    let parsed = parse_int_prefix(&String::from_utf8_lossy(&decoded));
    if parsed.is_finite() && parsed >= 0.0 { parsed } else { 0.0 }
}

pub fn decode_cursor_offset(cursor: Option<&str>) -> usize {
    let value = decode_cursor_number(cursor);
    if value.is_finite() && value >= 0.0 { value as usize } else { 0 }
}
