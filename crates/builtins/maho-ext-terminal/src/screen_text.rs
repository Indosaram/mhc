pub const DEFAULT_SCROLLBACK: usize = 1_000;
const MIN_SIZE: usize = 1;
const MAX_SIZE: usize = 10_000;
pub const MIN_REPLAY_HISTORY_LENGTH: usize = 4_096;

pub fn normalize_dimension(value: usize, fallback: usize) -> usize {
    if value == 0 {
        fallback.clamp(MIN_SIZE, MAX_SIZE)
    } else {
        value.clamp(MIN_SIZE, MAX_SIZE)
    }
}

pub const fn normalize_scrollback(value: usize) -> usize {
    value
}

pub fn normalize_replay_history_length(cols: usize, rows: usize, scrollback: usize) -> usize {
    cols.max(MIN_SIZE)
        .saturating_mul(rows.saturating_add(scrollback).saturating_add(1).max(MIN_SIZE))
        .saturating_mul(4)
        .max(MIN_REPLAY_HISTORY_LENGTH)
}

pub fn decode_input(value: &[u8]) -> String {
    String::from_utf8_lossy(value).into_owned()
}

pub fn sanitize_string(value: &str) -> String {
    value.to_owned()
}

pub fn sanitized_tail(value: &str, max_length: usize) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    String::from_utf16_lossy(&units[units.len().saturating_sub(max_length)..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_tail_is_owned_and_obeys_utf16_units() {
        assert_eq!(sanitized_tail("abc", 2), "bc");
        assert_eq!(sanitized_tail("a😀", 1), "�");
        assert_eq!(sanitized_tail("😀", 2), "😀");
    }

    #[test]
    fn replay_budget_has_upstream_bounds() {
        assert_eq!(normalize_replay_history_length(1, 1, 0), MIN_REPLAY_HISTORY_LENGTH);
        assert_eq!(normalize_replay_history_length(300, 80, 1_000), 1_297_200);
    }

    #[test]
    fn malformed_input_is_repaired_and_valid_text_is_preserved() {
        assert_eq!(decode_input(b"a\xffb"), "a�b");
        assert_eq!(sanitize_string("screen text"), "screen text");
    }
}
