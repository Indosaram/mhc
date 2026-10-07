//! Port of the pinned picomatch `lib/scan.js` (picomatch 4.0.5, MIT).

use super::constants::codes::{
    CHAR_ASTERISK, CHAR_AT, CHAR_BACKWARD_SLASH, CHAR_COMMA, CHAR_DOT, CHAR_EXCLAMATION_MARK, CHAR_FORWARD_SLASH,
    CHAR_LEFT_CURLY_BRACE, CHAR_LEFT_PARENTHESES, CHAR_LEFT_SQUARE_BRACKET, CHAR_PLUS, CHAR_QUESTION_MARK,
    CHAR_RIGHT_CURLY_BRACE, CHAR_RIGHT_PARENTHESES, CHAR_RIGHT_SQUARE_BRACKET,
};
use super::utils::{code_at, remove_backslashes};

#[derive(Clone, Copy, Debug, Default)]
pub struct ScanOptions {
    pub noext: bool,
    pub nonegate: bool,
    pub noparen: bool,
    pub parts: bool,
    pub tokens: bool,
    pub scan_to_end: bool,
    pub unescape: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ScanToken {
    pub value: String,
    pub depth: u64,
    pub is_glob: bool,
    pub is_prefix: bool,
    pub is_globstar: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ScanState {
    pub prefix: String,
    pub input: String,
    pub start: usize,
    pub base: String,
    pub glob: String,
    pub is_brace: bool,
    pub is_bracket: bool,
    pub is_glob: bool,
    pub is_extglob: bool,
    pub is_globstar: bool,
    pub negated: bool,
    pub negated_extglob: bool,
    pub max_depth: u64,
    pub tokens: Vec<ScanToken>,
    pub slashes: Vec<usize>,
    pub parts: Vec<String>,
}

fn is_path_separator(code: Option<u32>) -> bool {
    matches!(code, Some(CHAR_FORWARD_SLASH | CHAR_BACKWARD_SLASH))
}

fn assign_depth(token: &mut ScanToken) {
    if !token.is_prefix {
        token.depth = if token.is_globstar { u64::MAX } else { 1 };
    }
}

#[must_use]
pub fn scan(input: &str, options: &ScanOptions) -> ScanState {
    let chars: Vec<char> = input.chars().collect();
    let length = chars.len().saturating_sub(1);
    let scan_to_end = options.parts || options.scan_to_end;

    let mut slashes: Vec<usize> = Vec::new();
    let mut tokens: Vec<ScanToken> = Vec::new();
    let mut parts: Vec<String> = Vec::new();

    let mut str_slice = input.to_string();
    let mut index: isize = -1;
    let mut start = 0usize;
    let mut last_index = 0usize;
    let mut is_brace = false;
    let mut is_bracket = false;
    let mut is_glob = false;
    let mut is_extglob = false;
    let mut is_globstar = false;
    let mut brace_escaped = false;
    let mut backslashes = false;
    let mut negated = false;
    let mut negated_extglob = false;
    let mut finished = false;
    let mut braces = 0i64;
    let mut prev: Option<u32> = None;
    let mut code: Option<u32> = None;
    let mut token = ScanToken::default();

    let eos = |index: isize| index >= length as isize;
    let peek = |index: isize| code_at(&chars, (index + 1).max(0) as usize);

    while index < length as isize {
        prev = code;
        index += 1;
        code = code_at(&chars, index.max(0) as usize);

        if code == Some(CHAR_BACKWARD_SLASH) {
            backslashes = true;
            token.value.push('\\');
            index += 1;
            code = code_at(&chars, index.max(0) as usize);
            if code == Some(CHAR_LEFT_CURLY_BRACE) {
                brace_escaped = true;
            }
            continue;
        }

        if brace_escaped || code == Some(CHAR_LEFT_CURLY_BRACE) {
            braces += 1;
            loop {
                if eos(index) {
                    break;
                }
                index += 1;
                code = code_at(&chars, index.max(0) as usize);
                let Some(current) = code else { break };
                if current == CHAR_BACKWARD_SLASH {
                    backslashes = true;
                    index += 1;
                    continue;
                }
                if current == CHAR_LEFT_CURLY_BRACE {
                    braces += 1;
                    continue;
                }
                if !brace_escaped && current == CHAR_DOT {
                    index += 1;
                    code = code_at(&chars, index.max(0) as usize);
                    if code == Some(CHAR_DOT) {
                        is_brace = true;
                        is_glob = true;
                        finished = true;
                        if !scan_to_end {
                            break;
                        }
                        continue;
                    }
                }
                if !brace_escaped && current == CHAR_COMMA {
                    is_brace = true;
                    is_glob = true;
                    finished = true;
                    if !scan_to_end {
                        break;
                    }
                    continue;
                }
                if current == CHAR_RIGHT_CURLY_BRACE {
                    braces -= 1;
                    if braces == 0 {
                        brace_escaped = false;
                        is_brace = true;
                        finished = true;
                        break;
                    }
                }
            }
            if !scan_to_end {
                break;
            }
            continue;
        }

        if code == Some(CHAR_FORWARD_SLASH) {
            slashes.push(index.max(0) as usize);
            token.value.clear();
            tokens.push(token.clone());
            token = ScanToken::default();
            if finished {
                continue;
            }
            if prev == Some(CHAR_DOT) && index == (start as isize + 1) {
                start += 2;
                continue;
            }
            last_index = (index + 1).max(0) as usize;
            continue;
        }

        if !options.noext {
            let is_extglob_char = matches!(
                code,
                Some(CHAR_PLUS | CHAR_AT | CHAR_ASTERISK | CHAR_QUESTION_MARK | CHAR_EXCLAMATION_MARK)
            );
            if is_extglob_char && peek(index) == Some(CHAR_LEFT_PARENTHESES) {
                is_glob = true;
                is_extglob = true;
                finished = true;
                if code == Some(CHAR_EXCLAMATION_MARK) && index == start as isize {
                    negated_extglob = true;
                }
                if scan_to_end {
                    while !eos(index) {
                        index += 1;
                        code = code_at(&chars, index.max(0) as usize);
                        if code == Some(CHAR_BACKWARD_SLASH) {
                            backslashes = true;
                            index += 1;
                            continue;
                        }
                        if code == Some(CHAR_RIGHT_PARENTHESES) {
                            is_glob = true;
                            finished = true;
                            break;
                        }
                    }
                    continue;
                }
                break;
            }
        }

        if code == Some(CHAR_ASTERISK) {
            if prev == Some(CHAR_ASTERISK) {
                is_globstar = true;
            }
            is_glob = true;
            finished = true;
            if !scan_to_end {
                break;
            }
            continue;
        }

        if code == Some(CHAR_QUESTION_MARK) {
            is_glob = true;
            finished = true;
            if !scan_to_end {
                break;
            }
            continue;
        }

        if code == Some(CHAR_LEFT_SQUARE_BRACKET) {
            while !eos(index) {
                index += 1;
                let next = code_at(&chars, index.max(0) as usize);
                let Some(next_code) = next else { break };
                if next_code == CHAR_BACKWARD_SLASH {
                    backslashes = true;
                    index += 1;
                    continue;
                }
                if next_code == CHAR_RIGHT_SQUARE_BRACKET {
                    is_bracket = true;
                    is_glob = true;
                    finished = true;
                    break;
                }
            }
            if !scan_to_end {
                break;
            }
            continue;
        }

        if !options.nonegate && code == Some(CHAR_EXCLAMATION_MARK) && index == start as isize {
            negated = true;
            start += 1;
            continue;
        }

        if !options.noparen && code == Some(CHAR_LEFT_PARENTHESES) {
            is_glob = true;
            if scan_to_end {
                while !eos(index) {
                    index += 1;
                    code = code_at(&chars, index.max(0) as usize);
                    if code == Some(CHAR_LEFT_PARENTHESES) {
                        backslashes = true;
                        index += 1;
                        continue;
                    }
                    if code == Some(CHAR_RIGHT_PARENTHESES) {
                        finished = true;
                        break;
                    }
                }
                continue;
            }
            break;
        }

        if is_glob {
            finished = true;
            if !scan_to_end {
                break;
            }
            continue;
        }
    }

    if options.noext {
        is_extglob = false;
        is_glob = false;
    }

    let mut base = str_slice.clone();
    let mut prefix = String::new();
    let mut glob = String::new();

    if start > 0 {
        let units: Vec<char> = str_slice.chars().collect();
        prefix = units[..start].iter().collect();
        str_slice = units[start..].iter().collect();
        last_index = last_index.saturating_sub(start);
    }

    if !base.is_empty() && is_glob && last_index > 0 {
        let units: Vec<char> = str_slice.chars().collect();
        base = units[..last_index.min(units.len())].iter().collect();
        glob = units[last_index.min(units.len())..].iter().collect();
    } else if is_glob {
        base = String::new();
        glob = str_slice.clone();
    } else {
        base = str_slice.clone();
    }

    if !base.is_empty() && base != "/" && base != str_slice {
        let base_units: Vec<char> = base.chars().collect();
        if is_path_separator(code_at(&base_units, base_units.len().saturating_sub(1))) {
            base = base_units[..base_units.len().saturating_sub(1)].iter().collect();
        }
    }

    if options.unescape {
        if !glob.is_empty() {
            glob = remove_backslashes(&glob);
        }
        if !base.is_empty() && backslashes {
            base = remove_backslashes(&base);
        }
    }

    let mut state = ScanState {
        prefix,
        input: input.to_string(),
        start,
        base,
        glob,
        is_brace,
        is_bracket,
        is_glob,
        is_extglob,
        is_globstar,
        negated,
        negated_extglob,
        ..ScanState::default()
    };

    if options.tokens {
        if !is_path_separator(code) {
            tokens.push(token.clone());
        }
        state.tokens = tokens.clone();
    }

    if options.parts || options.tokens {
        let mut prev_index: Option<usize> = None;
        for idx in 0..slashes.len() {
            let n = prev_index.map_or(start, |value| value + 1);
            let i = slashes[idx];
            let units: Vec<char> = input.chars().collect();
            let value: String = units[n.min(units.len())..i.min(units.len())].iter().collect();
            if options.tokens {
                if idx == 0 && start != 0 {
                    if let Some(entry) = tokens.get_mut(idx) {
                        entry.is_prefix = true;
                        entry.value = state.prefix.clone();
                    }
                } else if let Some(entry) = tokens.get_mut(idx) {
                    entry.value = value.clone();
                }
                if let Some(entry) = tokens.get_mut(idx) {
                    assign_depth(entry);
                    state.max_depth += entry.depth;
                }
            }
            if idx != 0 || !value.is_empty() {
                parts.push(value);
            }
            prev_index = Some(i);
        }

        if let Some(prev_index) = prev_index
            && prev_index + 1 < input.chars().count()
        {
            let units: Vec<char> = input.chars().collect();
            let value: String = units[prev_index + 1..].iter().collect();
            parts.push(value.clone());
            if options.tokens
                && let Some(entry) = tokens.last_mut()
            {
                entry.value = value;
                assign_depth(entry);
                state.max_depth += entry.depth;
            }
        }

        state.slashes = slashes;
        state.parts = parts;
    }

    state
}
