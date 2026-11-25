//! Rewrites links inside memory notes that would dangle after a reflection/dream run removes or moves notes (#12642, #12645).

use std::collections::{BTreeMap, BTreeSet};

use crate::memfs::frontmatter_scalar::split_frontmatter;

pub const ARCHIVE_PATH: &str = "ARCHIVE.md";

/// Where a link to a path the run removed points now: the rename destination, the root archive, or nowhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Successor {
    Moved { path: String },
    Archived,
    Removed,
}

#[derive(Debug, Clone, Default)]
pub struct LinkTargets {
    pub live: BTreeSet<String>,
    pub removed: BTreeMap<String, Successor>,
}

/// Rewrites body links of the note at `path` that would dangle after the run: links to a removed
/// path go to its successor, and when the note itself moved from `moved_from`, its relative
/// markdown links re-resolve from the old folder. Fenced and inline code stay untouched.
pub fn repoint_links(
    path: &str,
    content: &str,
    targets: &LinkTargets,
    moved_from: Option<&str>,
) -> String {
    let body_start = match split_frontmatter(content) {
        Some((_, body)) => content.len().saturating_sub(body.len()),
        None => 0,
    };
    let mut fence: Option<(char, usize)> = None;
    let mut out_lines = Vec::new();
    for line in content[body_start..].split('\n') {
        if let Some((f_char, f_len)) = fence {
            if let Some((m_char, m_len, m_rest)) = parse_fence_marker(line)
                && m_char == f_char
                && m_len >= f_len
                && m_rest.trim().is_empty()
            {
                fence = None;
            }
            out_lines.push(line.to_owned());
            continue;
        }
        if let Some((m_char, m_len, _)) = parse_fence_marker(line) {
            fence = Some((m_char, m_len));
            out_lines.push(line.to_owned());
            continue;
        }
        out_lines.push(repoint_line_outside_code(path, line, targets, moved_from));
    }
    format!("{}{}", &content[..body_start], out_lines.join("\n"))
}

fn parse_fence_marker(line: &str) -> Option<(char, usize, &str)> {
    let mut spaces = 0usize;
    let mut rest = line;
    while let Some(next) = rest.strip_prefix(' ') {
        spaces += 1;
        if spaces > 3 {
            return None;
        }
        rest = next;
    }
    let ch = rest.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let len = rest.chars().take_while(|&c| c == ch).count();
    if len < 3 {
        return None;
    }
    Some((ch, len, &rest[len..]))
}

fn repoint_line_outside_code(
    path: &str,
    line: &str,
    targets: &LinkTargets,
    moved_from: Option<&str>,
) -> String {
    let mut out = String::with_capacity(line.len());
    let mut idx = 0usize;
    let bytes = line.as_bytes();
    while idx < line.len() {
        if bytes[idx] == b'`' {
            let tick_count = line[idx..].chars().take_while(|&c| c == '`').count();
            let ticks = &line[idx..idx + tick_count];
            if let Some(close_rel) = line[idx + tick_count..].find(ticks) {
                let end = idx + tick_count + close_rel + tick_count;
                out.push_str(&line[idx..end]);
                idx = end;
                continue;
            }
        }
        let next_tick = line[idx..].find('`').map_or(line.len(), |rel| idx + rel);
        out.push_str(&repoint_segment(
            path,
            &line[idx..next_tick],
            targets,
            moved_from,
        ));
        idx = next_tick;
    }
    out
}

fn repoint_segment(
    path: &str,
    text: &str,
    targets: &LinkTargets,
    moved_from: Option<&str>,
) -> String {
    let after_wiki = replace_wiki_links(text, targets);
    replace_markdown_links(path, &after_wiki, targets, moved_from)
}

fn replace_wiki_links(text: &str, targets: &LinkTargets) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end_rel) = after_open.find("]]") else {
            out.push_str("[[");
            rest = after_open;
            continue;
        };
        let inner = &after_open[..end_rel];
        if inner.contains('\n') {
            out.push_str("[[");
            rest = after_open;
            continue;
        }
        out.push_str(&rewrite_wiki_inner(inner, targets));
        rest = &after_open[end_rel + 2..];
    }
    out.push_str(rest);
    out
}

fn rewrite_wiki_inner(inner: &str, targets: &LinkTargets) -> String {
    let whole = format!("[[{inner}]]");
    let (target_part, alias) = match inner.split_once('|') {
        Some((t, a)) => (t, Some(a)),
        None => (inner, None),
    };
    let (raw_target, anchor) = match target_part.split_once('#') {
        Some((t, a)) => (t, format!("#{a}")),
        None => (target_part, String::new()),
    };
    let target = raw_target.trim();
    if !is_repo_relative(target) {
        return whole;
    }
    let resolved = posix_normalize(target);
    if escapes(&resolved) || resolves(&resolved, &targets.live, true) {
        return whole;
    }
    let (successor, had_exact) = match targets.removed.get(&resolved) {
        Some(s) => (s, true),
        None => match targets.removed.get(&format!("{resolved}.md")) {
            Some(s) => (s, false),
            None => return whole,
        },
    };
    let suffix = alias.map_or_else(String::new, |a| format!("|{a}"));
    match successor {
        Successor::Moved { path } => {
            let dest = if had_exact {
                path.as_str()
            } else {
                path.strip_suffix(".md").unwrap_or(path)
            };
            format!("[[{dest}{anchor}{suffix}]]")
        }
        Successor::Archived => {
            format!("[[{ARCHIVE_PATH}|{} (archived)]]", alias.unwrap_or(target))
        }
        Successor::Removed => alias.unwrap_or(target).to_owned(),
    }
}

fn replace_markdown_links(
    note_path: &str,
    text: &str,
    targets: &LinkTargets,
    moved_from: Option<&str>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let bytes = text.as_bytes();
    while i < text.len() {
        if bytes[i] == b'['
            && (i == 0 || bytes[i - 1] != b'!')
            && let Some(label_end_rel) = text[i + 1..].find(']')
        {
            let label_end = i + 1 + label_end_rel;
            let label = &text[i + 1..label_end];
            if !label.contains('\n') && text.as_bytes().get(label_end + 1) == Some(&b'(') {
                let dest_start = label_end + 2;
                if let Some(paren_end_rel) = text[dest_start..].find(')') {
                    let dest_end = dest_start + paren_end_rel;
                    let raw_dest = &text[dest_start..dest_end];
                    if !raw_dest.is_empty() && !raw_dest.contains('\n') {
                        let rewritten =
                            rewrite_markdown_match(note_path, label, raw_dest, targets, moved_from);
                        out.push_str(&rewritten);
                        i = dest_end + 1;
                        continue;
                    }
                }
            }
        }
        let ch = text[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn rewrite_markdown_match(
    note_path: &str,
    label: &str,
    raw_dest: &str,
    targets: &LinkTargets,
    moved_from: Option<&str>,
) -> String {
    let whole = format!("[{label}]({raw_dest})");
    let destination = markdown_destination(raw_dest);
    let (destination_path, anchor) = match destination.split_once('#') {
        Some((p, a)) => (p, format!("#{a}")),
        None => (destination, String::new()),
    };
    if !is_repo_relative(destination_path) {
        return whole;
    }
    let origin_base = posix_dirname(moved_from.unwrap_or(note_path));
    let original = posix_normalize(&posix_join(origin_base, destination_path));
    if escapes(&original) {
        return whole;
    }
    let successor = targets.removed.get(&original);
    if matches!(successor, Some(Successor::Removed)) {
        return label.to_owned();
    }
    let next_target = match successor {
        None => {
            if resolves(&original, &targets.live, false) {
                Some(original.as_str())
            } else {
                None
            }
        }
        Some(Successor::Moved { path }) => Some(path.as_str()),
        Some(Successor::Archived) => Some(ARCHIVE_PATH),
        Some(Successor::Removed) => unreachable!(),
    };
    let Some(next_target) = next_target else {
        return whole;
    };
    let cur_resolved = posix_normalize(&posix_join(posix_dirname(note_path), destination_path));
    if next_target == cur_resolved {
        return whole;
    }
    let rel = posix_relative(posix_dirname(note_path), next_target);
    let next = if matches!(successor, Some(Successor::Archived)) {
        rel
    } else {
        format!("{rel}{anchor}")
    };
    let replaced = if next.chars().any(char::is_whitespace) && !raw_dest.trim().starts_with('<') {
        format!("<{next}>")
    } else {
        next
    };
    let new_raw = raw_dest.replacen(destination, &replaced, 1);
    format!("[{label}]({new_raw})")
}

fn is_repo_relative(target: &str) -> bool {
    if target.is_empty() || target.starts_with('/') {
        return false;
    }
    if let Some((scheme, _)) = target.split_once(':') {
        let mut chars = scheme.chars();
        if chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        {
            return false;
        }
    }
    true
}

fn escapes(resolved: &str) -> bool {
    resolved == ".." || resolved.starts_with("../")
}

fn resolves(resolved: &str, live: &BTreeSet<String>, wiki: bool) -> bool {
    if live.contains(resolved) || (wiki && live.contains(&format!("{resolved}.md"))) {
        return true;
    }
    let prefix = if resolved.ends_with('/') {
        resolved.to_owned()
    } else {
        format!("{resolved}/")
    };
    live.iter().any(|file| file.starts_with(&prefix))
}

fn markdown_destination(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(inner) = trimmed.strip_prefix('<') {
        return match inner.find('>') {
            Some(end) => &inner[..end],
            None => inner,
        };
    }
    trimmed.split_whitespace().next().unwrap_or("")
}

pub(crate) fn posix_dirname(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((dir, _)) => dir,
        None => ".",
    }
}

pub(crate) fn posix_basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn posix_join(base: &str, rel: &str) -> String {
    if rel.starts_with('/') {
        return rel.to_owned();
    }
    if base.is_empty() || base == "." {
        rel.to_owned()
    } else if base.ends_with('/') {
        format!("{base}{rel}")
    } else {
        format!("{base}/{rel}")
    }
}

fn posix_normalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            if let Some(&last) = parts.last()
                && last != ".."
            {
                parts.pop();
            } else if !absolute {
                parts.push("..");
            }
        } else {
            parts.push(seg);
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_owned()
    } else {
        joined
    }
}

fn posix_relative(from: &str, to: &str) -> String {
    let from_norm = posix_normalize(from);
    let from_parts: Vec<&str> = from_norm
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let to_norm = posix_normalize(to);
    let to_parts: Vec<&str> = to_norm
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let mut common = 0usize;
    while common < from_parts.len()
        && common < to_parts.len()
        && from_parts[common] == to_parts[common]
    {
        common += 1;
    }
    let mut rel: Vec<&str> = vec![".."; from_parts.len() - common];
    rel.extend_from_slice(&to_parts[common..]);
    if rel.is_empty() {
        ".".to_owned()
    } else {
        rel.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repoints_moved_archived_and_deleted_links_while_preserving_code_spans() {
        let mut live = BTreeSet::new();
        live.insert("ARCHIVE.md".to_owned());
        live.insert("notes/new.md".to_owned());
        live.insert("notes/index.md".to_owned());
        let mut removed = BTreeMap::new();
        removed.insert(
            "notes/old.md".to_owned(),
            Successor::Moved {
                path: "notes/new.md".to_owned(),
            },
        );
        removed.insert("notes/archived.md".to_owned(), Successor::Archived);
        removed.insert("notes/deleted.md".to_owned(), Successor::Removed);

        let content = "---\ndescription: Index\n---\nSee [old](old.md#sec), [arch](archived.md#part), [gone](deleted.md), [[notes/old|Old]], [[notes/archived|Arch]], `[[notes/old]]`.\n```md\n[keep](old.md)\n```\n";
        let updated = repoint_links(
            "notes/index.md",
            content,
            &LinkTargets { live, removed },
            None,
        );
        assert!(updated.contains("[old](new.md#sec)"));
        assert!(updated.contains("[arch](../ARCHIVE.md)"));
        assert!(updated.contains("gone,"));
        assert!(updated.contains("[[notes/new|Old]]"));
        assert!(updated.contains("[[ARCHIVE.md|Arch (archived)]]"));
        assert!(updated.contains("`[[notes/old]]`"));
        assert!(updated.contains("```md\n[keep](old.md)\n```"));
    }
}
