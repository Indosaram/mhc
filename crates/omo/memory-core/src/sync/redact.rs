//! Credential redaction for memory-repository URLs.

const MASK: &str = "***";

fn is_scheme_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '+' || character == '.' || character == '-'
}

fn redact_url_userinfo(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    let _bytes = input.as_bytes();

    while let Some(rel_idx) = input[cursor..].find("://") {
        let scheme_delim_idx = cursor + rel_idx;

        let before_delim = &input[..scheme_delim_idx];
        let mut scheme_start = scheme_delim_idx;
        while scheme_start > 0 {
            let prev_char = before_delim[..scheme_start].chars().next_back().unwrap();
            if is_scheme_char(prev_char) {
                scheme_start -= prev_char.len_utf8();
            } else {
                break;
            }
        }

        let scheme_candidate = &input[scheme_start..scheme_delim_idx];
        let valid_scheme = !scheme_candidate.is_empty()
            && scheme_candidate
                .chars()
                .next()
                .unwrap()
                .is_ascii_alphabetic();

        if !valid_scheme {
            out.push_str(&input[cursor..scheme_delim_idx + 3]);
            cursor = scheme_delim_idx + 3;
            continue;
        }

        let after_scheme_idx = scheme_delim_idx + 3;
        let mut userinfo_end = None;
        let mut colon_pos = None;

        for (offset, character) in input[after_scheme_idx..].char_indices() {
            let current_pos = after_scheme_idx + offset;
            if character == '/' || character.is_whitespace() {
                break;
            }
            if character == ':' && colon_pos.is_none() {
                colon_pos = Some(current_pos);
                continue;
            }
            if character == '@' {
                userinfo_end = Some(current_pos);
                break;
            }
        }

        if let Some(at_idx) = userinfo_end {
            let user_part = match colon_pos {
                Some(c_idx) => &input[after_scheme_idx..c_idx],
                None => &input[after_scheme_idx..at_idx],
            };

            if !user_part.is_empty() {
                out.push_str(&input[cursor..scheme_start]);
                out.push_str(&input[scheme_start..after_scheme_idx]);
                if colon_pos.is_some() {
                    out.push_str(MASK);
                    out.push(':');
                    out.push_str(MASK);
                    out.push('@');
                } else {
                    out.push_str(MASK);
                    out.push('@');
                }
                cursor = at_idx + 1;
                continue;
            }
        }

        out.push_str(&input[cursor..after_scheme_idx]);
        cursor = after_scheme_idx;
    }

    out.push_str(&input[cursor..]);
    out
}

fn redact_scp_userinfo(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(rel_idx) = input[cursor..].find('@') {
        let at_idx = cursor + rel_idx;

        let _before_at = &input[cursor..at_idx];
        let mut user_start = at_idx;
        while user_start > cursor {
            let prev_char = input[..user_start].chars().next_back().unwrap();
            if prev_char.is_whitespace() || prev_char == ':' || prev_char == '/' || prev_char == '@'
            {
                break;
            }
            user_start -= prev_char.len_utf8();
        }

        let user_part = &input[user_start..at_idx];
        if user_part.is_empty() {
            out.push_str(&input[cursor..=at_idx]);
            cursor = at_idx + 1;
            continue;
        }

        let valid_prefix = if user_start == 0 {
            true
        } else {
            let prefix_char = input[..user_start].chars().next_back().unwrap();
            prefix_char.is_whitespace()
                || prefix_char == '\''
                || prefix_char == '"'
                || prefix_char == '('
                || prefix_char == '<'
        };

        if !valid_prefix {
            out.push_str(&input[cursor..=at_idx]);
            cursor = at_idx + 1;
            continue;
        }

        let after_at = &input[at_idx + 1..];
        let mut host_len = 0;
        let mut colon_found = false;

        for (offset, character) in after_at.char_indices() {
            if character == ':' {
                host_len = offset;
                colon_found = true;
                break;
            }
            if character.is_whitespace() || character == '/' || character == '@' {
                break;
            }
        }

        if !colon_found || host_len == 0 {
            out.push_str(&input[cursor..=at_idx]);
            cursor = at_idx + 1;
            continue;
        }

        let colon_idx = at_idx + 1 + host_len;
        let after_colon = &input[colon_idx + 1..];

        let mut has_slash_before_whitespace = false;
        for character in after_colon.chars() {
            if character.is_whitespace() {
                break;
            }
            if character == '/' {
                has_slash_before_whitespace = true;
                break;
            }
        }

        if !has_slash_before_whitespace {
            out.push_str(&input[cursor..=at_idx]);
            cursor = at_idx + 1;
            continue;
        }

        out.push_str(&input[cursor..user_start]);
        out.push_str(MASK);
        out.push('@');
        out.push_str(&input[at_idx + 1..=colon_idx]);
        cursor = colon_idx + 1;
    }

    out.push_str(&input[cursor..]);
    out
}

/// Mask credentials in a URL, or in free text containing URLs.
pub fn redact_url(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let step1 = redact_url_userinfo(value);
    redact_scp_userinfo(&step1)
}

#[cfg(test)]
#[path = "redact_tests.rs"]
mod tests;
