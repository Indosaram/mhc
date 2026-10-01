//! Port of senpi packages/agent/src/harness/utils/truncate.ts.

/// `DEFAULT_MAX_LINES`.
pub const DEFAULT_MAX_LINES: u64 = 2000;

/// `DEFAULT_MAX_BYTES`.
pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024;

/// `GREP_MAX_LINE_LENGTH`.
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// `TruncationResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<TruncatedBy>,
    pub total_lines: u64,
    pub total_bytes: u64,
    pub output_lines: u64,
    pub output_bytes: u64,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: u64,
    pub max_bytes: u64,
}

/// `"lines" | "bytes"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

/// `TruncationOptions`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TruncationOptions {
    pub max_lines: Option<u64>,
    pub max_bytes: Option<u64>,
}

/// `utf8ByteLength(content)`: UTF-8 byte length. Node's `Buffer.byteLength` path, so astral
/// characters count as 4 bytes.
pub fn utf8_byte_length(content: &str) -> u64 {
    content.len() as u64
}

fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// `formatSize(bytes)`.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// `truncateHead(content, options)`.
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = utf8_byte_length(content);
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len() as u64;

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let first_line_bytes = lines.first().map_or(0, |line| utf8_byte_length(line));
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines_arr: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0u64;
    let mut truncated_by = TruncatedBy::Lines;

    for (index, line) in lines.iter().enumerate() {
        if index as u64 >= max_lines {
            break;
        }
        let line_bytes = utf8_byte_length(line) + if index > 0 { 1 } else { 0 };
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output_lines_arr.push(line);
        output_bytes_count += line_bytes;
    }

    if output_lines_arr.len() as u64 >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = utf8_byte_length(&output_content);

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines_arr.len() as u64,
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// `truncateTail(content, options)`.
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = utf8_byte_length(content);
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len() as u64;

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines_arr: Vec<String> = Vec::new();
    let mut output_bytes_count = 0u64;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    for line in lines.iter().rev() {
        if output_lines_arr.len() as u64 >= max_lines {
            break;
        }
        let line_bytes = utf8_byte_length(line) + if !output_lines_arr.is_empty() { 1 } else { 0 };
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            if output_lines_arr.is_empty() {
                let truncated_line = truncate_string_to_bytes_from_end(line, max_bytes);
                output_bytes_count = utf8_byte_length(&truncated_line);
                output_lines_arr.insert(0, truncated_line);
                last_line_partial = true;
            }
            break;
        }
        output_lines_arr.insert(0, (*line).to_string());
        output_bytes_count += line_bytes;
    }

    if output_lines_arr.len() as u64 >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = utf8_byte_length(&output_content);

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines_arr.len() as u64,
        output_bytes: final_output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

fn replace_unpaired_surrogates(units: &[u16]) -> String {
    let mut output = String::new();
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if (0xd800..=0xdbff).contains(&unit) {
            if index + 1 < units.len() && (0xdc00..=0xdfff).contains(&units[index + 1]) {
                output.push_str(&String::from_utf16_lossy(&units[index..index + 2]));
                index += 2;
                continue;
            }
            output.push('\u{fffd}');
        } else if (0xdc00..=0xdfff).contains(&unit) {
            output.push('\u{fffd}');
        } else {
            output.push_str(&String::from_utf16_lossy(&[unit]));
        }
        index += 1;
    }
    output
}

/// `truncateStringToBytesFromEnd(str, maxBytes)`.
fn truncate_string_to_bytes_from_end(value: &str, max_bytes: u64) -> String {
    if max_bytes == 0 {
        return String::new();
    }

    let units: Vec<u16> = value.encode_utf16().collect();
    let mut output_bytes = 0u64;
    let mut start = units.len();
    let mut needs_replacement = false;
    let mut index = units.len();
    while index > 0 {
        let mut character_start = index - 1;
        let code = units[character_start];
        let character_bytes: u64;
        let mut unpaired_surrogate = false;
        if (0xdc00..=0xdfff).contains(&code) && character_start > 0 {
            let previous = units[character_start - 1];
            if (0xd800..=0xdbff).contains(&previous) {
                character_start -= 1;
                character_bytes = 4;
            } else {
                character_bytes = 3;
                unpaired_surrogate = true;
            }
        } else if (0xd800..=0xdfff).contains(&code) {
            character_bytes = 3;
            unpaired_surrogate = true;
        } else {
            character_bytes = if code <= 0x007f {
                1
            } else if code <= 0x07ff {
                2
            } else {
                3
            };
        }
        if output_bytes + character_bytes > max_bytes {
            break;
        }
        output_bytes += character_bytes;
        start = character_start;
        needs_replacement = needs_replacement || unpaired_surrogate;
        index = character_start;
    }

    let slice = &units[start..];
    if needs_replacement {
        replace_unpaired_surrogates(slice)
    } else {
        String::from_utf16_lossy(slice)
    }
}

/// `truncateLine(line, maxChars)`.
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    let length = line.chars().count();
    if length <= max_chars {
        return (line.to_string(), false);
    }
    let head: String = line.chars().take(max_chars).collect();
    (format!("{head}... [truncated]"), true)
}
