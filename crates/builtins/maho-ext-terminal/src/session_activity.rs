//! Port of senpi `terminal/session-activity.ts`: the newest transcript timestamp strictly before
//! `before_ms`, read from the tail of the session JSONL.
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const TAIL_BYTES: u64 = 256 * 1024;

/// `sessionActivityBeforeMs`. `None` when the file is absent or carries no earlier stamp.
pub fn session_activity_before_ms(
    session_file: Option<&Path>,
    before_ms: f64,
) -> std::io::Result<Option<f64>> {
    let Some(session_file) = session_file else {
        return Ok(None);
    };
    let mut file = match std::fs::File::open(session_file) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let size = file.metadata()?.len();
    let length = size.min(TAIL_BYTES);
    file.seek(SeekFrom::Start(size - length))?;
    let mut buffer = Vec::with_capacity(length as usize);
    file.take(length).read_to_end(&mut buffer)?;
    let text = String::from_utf8_lossy(&buffer);
    let matcher = regex::Regex::new(r#""timestamp":"([^"]+)""#).map_err(std::io::Error::other)?;
    let mut newest: Option<f64> = None;
    for capture in matcher.captures_iter(&text) {
        let Some(stamp) = parse_timestamp_ms(&capture[1]) else {
            continue;
        };
        if stamp >= before_ms {
            continue;
        }
        if newest.is_none_or(|current| stamp > current) {
            newest = Some(stamp);
        }
    }
    Ok(newest)
}

/// The ISO-8601 subset senpi writes (`Date.toISOString`, RFC 3339 with offset); other `Date.parse`
/// forms never appear in session JSONL.
fn parse_timestamp_ms(text: &str) -> Option<f64> {
    let (date, time) = text.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (clock, offset_ms) = if let Some(clock) = time.strip_suffix('Z') {
        (clock, 0)
    } else {
        let split = time.rfind(['+', '-'])?;
        let (clock, offset) = time.split_at(split);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = offset[1..].split_once(':')?;
        let hours: i64 = hours.parse().ok()?;
        let minutes: i64 = minutes.parse().ok()?;
        (clock, sign * (hours * 3_600_000 + minutes * 60_000))
    };
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let seconds: f64 = clock_parts.next()?.parse().ok()?;
    if clock_parts.next().is_some() || hour > 23 || minute > 59 || !(0.0..61.0).contains(&seconds) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let whole = (days * 86_400 + hour * 3_600 + minute * 60) * 1_000 - offset_ms;
    Some(whole as f64 + (seconds * 1000.0).floor())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_stamp_strictly_before_cutoff() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("s.jsonl");
        std::fs::write(
            &file,
            concat!(
                "{\"timestamp\":\"2026-01-01T00:00:01.000Z\"}\n",
                "{\"timestamp\":\"2026-01-01T00:00:03.500Z\"}\n",
                "{\"timestamp\":\"2026-01-01T00:00:09.000Z\"}\n",
                "{\"timestamp\":\"garbage\"}\n"
            ),
        )?;
        let base = parse_timestamp_ms("2026-01-01T00:00:00.000Z").expect("base");
        assert_eq!(base, 1_767_225_600_000.0);
        assert_eq!(
            session_activity_before_ms(Some(&file), base + 5_000.0)?,
            Some(base + 3_500.0)
        );
        assert_eq!(session_activity_before_ms(Some(&file), base)?, None);
        assert_eq!(
            session_activity_before_ms(Some(&dir.path().join("missing")), base)?,
            None
        );
        assert_eq!(session_activity_before_ms(None, base)?, None);
        assert_eq!(parse_timestamp_ms("2026-01-01T09:00:00+09:00"), Some(base));
        Ok(())
    }
}
