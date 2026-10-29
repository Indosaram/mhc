//! People card format parsing, serialization, and slug resolution.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Valid prefixes for people card entries.
pub const VALID_PREFIXES: [&str; 4] = ["IDENTITY", "ATTRIBUTE", "RELATIONSHIP", "INSTRUCTION"];

/// Valid section headings for observations.
pub const VALID_SECTIONS: [&str; 2] = ["Explicit", "Deductive"];

/// A single entry in a people card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardEntry {
    pub prefix: String,
    pub content: String,
}

/// A structured observation entry with optional provenance and metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ObservationEntry {
    pub date: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// A section group of observation entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationGroup {
    pub section: String,
    pub entries: Vec<ObservationEntry>,
}

/// A complete people card with structured entries and observation groups.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PeopleCard {
    pub entries: Vec<CardEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observations: Option<Vec<ObservationGroup>>,
}

/// Parsing limits applied to people cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeopleLimits {
    pub max_entries: usize,
    pub max_entry_chars: usize,
}

/// The result of parsing a people card text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeopleCardParseResult {
    pub card: PeopleCard,
    pub diagnostics: Vec<String>,
}

/// Parse a people card from markdown text.
pub fn parse_people_card(text: &str, limits: PeopleLimits) -> PeopleCardParseResult {
    let mut diagnostics = Vec::new();
    let mut entries = Vec::new();
    let mut observation_groups = Vec::new();
    let mut current_section: Option<ObservationGroup> = None;

    let lines: Vec<&str> = text.split('\n').collect();

    for (index, raw_line) in lines.iter().enumerate() {
        let line_num = index + 1;
        let line = raw_line.trim();

        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') && !line.starts_with("##") {
            continue;
        }

        if let Some(section_header) = line.strip_prefix("## ") {
            let section_name = section_header.trim();
            if !VALID_SECTIONS.contains(&section_name) {
                diagnostics.push(format!(
                    "Line {line_num}: unknown observation section \"## {section_name}\" (expected ## Explicit or ## Deductive)"
                ));
            }
            if let Some(prev) = current_section.take() {
                observation_groups.push(prev);
            }
            current_section = Some(ObservationGroup {
                section: section_name.to_string(),
                entries: Vec::new(),
            });
            continue;
        }

        if let Some(section) = &mut current_section {
            if !line.starts_with("- ") {
                diagnostics.push(format!(
                    "Line {line_num}: observation entry must start with \"- [YYYY-MM-DD]\": {line}"
                ));
                continue;
            }

            let after_dash = line[2..].trim_start();
            let parsed_date = parse_date_entry(after_dash);
            let (date, mut rest) = match parsed_date {
                Some((d, r)) => (d, r),
                None => {
                    diagnostics.push(format!(
                        "Line {line_num}: observation entry missing valid date bracket: {line}"
                    ));
                    continue;
                }
            };

            let comment_fields = if let Some((c_start, c_end)) = find_comment(&rest) {
                let comment_raw = &rest[c_start + 4..c_end];
                let fields = parse_comment_fields(comment_raw);
                let before = &rest[..c_start];
                let after = &rest[c_end + 3..];
                rest = format!("{before}{after}").trim().to_string();
                fields
            } else {
                ObservationEntry::default()
            };

            section.entries.push(ObservationEntry {
                date: date.to_string(),
                content: rest,
                src: comment_fields.src,
                n: comment_fields.n,
                pattern: comment_fields.pattern,
                confidence: comment_fields.confidence,
                status: comment_fields.status,
            });
            continue;
        }

        let colon_idx = match line.find(':') {
            Some(idx) => idx,
            None => {
                diagnostics.push(format!(
                    "Line {line_num}: malformed line, expected PREFIX: content"
                ));
                continue;
            }
        };

        let prefix = line[..colon_idx].trim();
        let content = line[colon_idx + 1..].trim();

        if !VALID_PREFIXES.contains(&prefix) {
            diagnostics.push(format!(
                "Line {line_num}: unknown prefix \"{prefix}\" (must be one of: {})",
                VALID_PREFIXES.join(", ")
            ));
            continue;
        }

        if content.is_empty() {
            diagnostics.push(format!(
                "Line {line_num}: empty content for prefix \"{prefix}\""
            ));
            continue;
        }

        if content.chars().count() > limits.max_entry_chars {
            diagnostics.push(format!(
                "Line {line_num}: entry exceeds max length of {} chars ({})",
                limits.max_entry_chars,
                content.chars().count()
            ));
        }

        entries.push(CardEntry {
            prefix: prefix.to_string(),
            content: content.to_string(),
        });
    }

    if let Some(prev) = current_section {
        observation_groups.push(prev);
    }

    if entries.len() > limits.max_entries {
        diagnostics.push(format!(
            "Card contains {} entries, exceeding maximum of {}",
            entries.len(),
            limits.max_entries
        ));
    }

    let card = PeopleCard {
        entries,
        observations: if observation_groups.is_empty() {
            None
        } else {
            Some(observation_groups)
        },
    };

    PeopleCardParseResult { card, diagnostics }
}

/// Serialize a people card into markdown text.
pub fn serialize_people_card(card: &PeopleCard, _limits: PeopleLimits) -> String {
    let mut parts = Vec::new();

    if !card.entries.is_empty() {
        let entry_lines: Vec<String> = card
            .entries
            .iter()
            .map(|entry| format!("{}: {}", entry.prefix, entry.content))
            .collect();
        parts.push(entry_lines.join("\n"));
    }

    if let Some(observations) = &card.observations {
        for group in observations {
            let mut group_lines = Vec::new();
            group_lines.push(format!("## {}", group.section));
            for entry in &group.entries {
                let mut comment_parts = Vec::new();
                if let Some(src) = &entry.src {
                    comment_parts.push(format!("src: {src}"));
                }
                if let Some(n) = entry.n {
                    comment_parts.push(format!("n={n}"));
                }
                if let Some(pattern) = &entry.pattern {
                    comment_parts.push(format!("pattern: {pattern}"));
                }
                if let Some(confidence) = &entry.confidence {
                    comment_parts.push(format!("confidence: {confidence}"));
                }
                if let Some(status) = &entry.status {
                    comment_parts.push(format!("status: {status}"));
                }

                let comment = if comment_parts.is_empty() {
                    String::new()
                } else {
                    format!(" <!-- {} -->", comment_parts.join("; "))
                };
                group_lines.push(format!("- [{}] {}{comment}", entry.date, entry.content));
            }
            parts.push(group_lines.join("\n"));
        }
    }

    parts.join("\n\n")
}

/// Parse metadata fields from an HTML comment string inside an observation entry.
pub fn parse_comment_fields(raw: &str) -> ObservationEntry {
    let mut entry = ObservationEntry::default();
    for pair in raw.split(';') {
        let pair = pair.trim();
        let colon_idx = pair.find(':');
        let eq_idx = pair.find('=');
        let sep_idx = match (colon_idx, eq_idx) {
            (Some(colon), Some(eq)) => Some(colon.min(eq)),
            (Some(colon), None) => Some(colon),
            (None, Some(eq)) => Some(eq),
            (None, None) => None,
        };
        let sep_idx = match sep_idx {
            Some(idx) => idx,
            None => continue,
        };

        let key = pair[..sep_idx].trim();
        let val = pair[sep_idx + 1..].trim();
        if key.is_empty() || val.is_empty() {
            continue;
        }

        if key == "src" {
            entry.src = Some(val.to_string());
        } else if key == "n" {
            if let Ok(num) = val.parse::<u64>() {
                entry.n = Some(num);
            }
        } else if key == "pattern" {
            entry.pattern = Some(val.to_string());
        } else if key == "confidence" {
            entry.confidence = Some(val.to_string());
        } else if key == "status" {
            entry.status = Some(val.to_string());
        }
    }
    entry
}

/// Sanitize a display name into a canonical slug.
pub fn sanitize_person_slug(display_name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in display_name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    if slug.ends_with('-') {
        slug.pop();
    }
    if slug.chars().count() > 40 {
        let mut truncated: String = slug.chars().take(40).collect();
        if truncated.ends_with('-') {
            truncated.pop();
        }
        truncated
    } else {
        slug
    }
}

/// Check whether a slug is reserved.
pub fn is_reserved_slug(slug: &str) -> bool {
    slug.eq_ignore_ascii_case("human")
}

/// Resolve a slug collision by appending numeric increments (`-2`, `-3`, etc.).
pub fn resolve_slug_collision(base_slug: &str, existing_slugs: &HashSet<String>) -> String {
    if !existing_slugs.contains(base_slug) {
        return base_slug.to_string();
    }

    let mut counter = 2;
    loop {
        let candidate = format!("{base_slug}-{counter}");
        if !existing_slugs.contains(&candidate) {
            return candidate;
        }
        counter += 1;
    }
}

fn parse_date_entry(input: &str) -> Option<(&str, String)> {
    if !input.starts_with('[') {
        return None;
    }
    let close_idx = input.find(']')?;
    let date_str = &input[1..close_idx];
    if !is_valid_date_format(date_str) {
        return None;
    }

    let rest = input[close_idx + 1..].trim_start();
    Some((date_str, rest.to_string()))
}

fn is_valid_date_format(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 10 {
        return false;
    }
    bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

fn find_comment(input: &str) -> Option<(usize, usize)> {
    let start = input.find("<!--")?;
    let end = input[start + 4..].find("-->")?;
    Some((start, start + 4 + end))
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
