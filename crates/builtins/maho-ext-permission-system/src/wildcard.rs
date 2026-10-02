pub fn matches(value: &str, pattern: &str) -> bool {
    let value: Vec<u16> = value.encode_utf16().collect();
    let pattern: Vec<u16> = pattern.encode_utf16().collect();
    let (mut value_idx, mut pattern_idx, mut match_idx) = (0, 0, 0);
    let mut star_idx = None;
    while value_idx < value.len() {
        match pattern.get(pattern_idx).copied() {
            Some(63) => { value_idx += 1; pattern_idx += 1; }
            Some(42) => { star_idx = Some(pattern_idx); match_idx = value_idx; pattern_idx += 1; }
            Some(unit) if Some(&unit) == value.get(value_idx) => { value_idx += 1; pattern_idx += 1; }
            _ => match star_idx {
                Some(star) => { pattern_idx = star + 1; match_idx += 1; value_idx = match_idx; }
                None => return false,
            },
        }
    }
    while pattern.get(pattern_idx) == Some(&42) { pattern_idx += 1; }
    pattern_idx == pattern.len()
}
