//! Broken-link detection for a memory corpus.
//!
//! Spec: `docs/cleanroom/specs/memory-broken-links.md`. A memory store is a wiki: files point at each other
//! with `[[name]]`, and a rename that misses one referrer leaves a link that silently resolves to nothing.
//!
//! Scope is link *extraction and resolution* only. Walking the corpus, reading files and building the report
//! live at the boundary, because they need I/O and this module must be testable without a filesystem.

use std::collections::BTreeSet;

/// A link written in a note body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteLink {
    pub target: String,
    pub wiki: bool,
}

/// Every link the reader would see, in document order.
///
/// Fenced code blocks, inline code spans and images are skipped. None of the three is a link: a fenced block
/// is an example, an inline span is literal text, and an image destination is an asset path. Reporting those
/// as broken links would make the check useless on any note that documents markdown.
pub fn note_links(body: &str) -> Vec<NoteLink> {
    let mut links = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in body.lines() {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        let marker = trimmed.chars().next().filter(|ch| *ch == '`' || *ch == '~');
        let run = marker.map_or(0, |ch| trimmed.chars().take_while(|x| *x == ch).count());
        if let Some((fence_char, fence_len)) = fence {
            if indent <= 3
                && marker == Some(fence_char)
                && run >= fence_len
                && trimmed[run..].trim().is_empty()
            {
                fence = None;
            }
            continue;
        }
        if indent <= 3 && run >= 3 {
            fence = marker.map(|ch| (ch, run));
            continue;
        }
        let text = strip_code_spans(line);
        links.extend(wiki_links(&text));
        links.extend(inline_links(&text));
    }
    links
}

/// The line with every inline code span removed, so `` `[[not a link]]` `` is not a link.
///
/// An unmatched backtick is a literal character in markdown, not the start of a span, so the remainder stays
/// in the text and a link after it is still a link.
fn strip_code_spans(line: &str) -> String {
    let mut text = String::new();
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        text.push_str(&rest[..open]);
        let run = rest[open..]
            .bytes()
            .take_while(|byte| *byte == b'`')
            .count();
        let delimiter = "`".repeat(run);
        if let Some(end) = rest[open + run..].find(&delimiter) {
            rest = &rest[open + run + end + run..];
        } else {
            text.push_str(&rest[open..]);
            rest = "";
            break;
        }
    }
    text.push_str(rest);
    text
}

fn wiki_links(text: &str) -> Vec<NoteLink> {
    let mut links = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("[[") {
        rest = &rest[open + 2..];
        let Some(end) = rest.find("]]") else {
            break;
        };
        let target = rest[..end].split('|').next().unwrap_or("").to_owned();
        links.push(NoteLink { target, wiki: true });
        rest = &rest[end + 2..];
    }
    links
}

fn inline_links(text: &str) -> Vec<NoteLink> {
    let mut links = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let is_image = open > 0 && rest.as_bytes()[open - 1] == b'!';
        rest = &rest[open + 1..];
        let Some(label_end) = rest.find("](") else {
            continue;
        };
        let after = &rest[label_end + 2..];
        let Some(close) = after.find(')') else {
            break;
        };
        if !is_image {
            let raw = after[..close].trim();
            let destination = if let Some(angle) = raw.strip_prefix('<') {
                angle.split('>').next().unwrap_or(angle)
            } else {
                raw.split_whitespace().next().unwrap_or("")
            };
            links.push(NoteLink {
                target: destination.to_owned(),
                wiki: false,
            });
        }
        rest = &after[close + 1..];
    }
    links
}

pub fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanglingLink {
    pub target: String,
    /// The link climbs out of the repository. Reported separately because it is a different mistake: not a
    /// rename that missed a referrer, but a note pointing at something outside the store.
    pub escapes: bool,
}

/// Links in `body` that resolve to no known path.
///
/// `existing` holds repository-relative file paths. A link is satisfied by an exact file, by `<path>.md` for
/// a wiki link (that is how `[[name]]` addresses a note), or by a directory that contains anything.
pub fn dangling_links(path: &str, body: &str, existing: &BTreeSet<String>) -> Vec<DanglingLink> {
    let mut dangling = Vec::new();
    for link in note_links(body) {
        let target = link.target.split('#').next().unwrap_or("").trim();
        let lower = target.to_ascii_lowercase();
        if target.is_empty()
            || lower.starts_with("https:")
            || lower.starts_with("http:")
            || lower.starts_with("mailto:")
        {
            continue;
        }
        let joined = if link.wiki {
            target.to_owned()
        } else {
            let directory = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            format!("{directory}/{target}")
        };
        let resolved = normalize_path(&joined);
        let escapes = target.starts_with('/') || resolved == ".." || resolved.starts_with("../");
        let satisfied = existing.contains(&resolved)
            || (link.wiki && existing.contains(&format!("{resolved}.md")))
            || existing
                .iter()
                .any(|candidate| candidate.starts_with(&format!("{resolved}/")));
        if !escapes && satisfied {
            continue;
        }
        dangling.push(DanglingLink {
            target: target.to_owned(),
            escapes,
        });
    }
    dangling
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn a_wiki_link_is_extracted() {
        let links = note_links("see [[people/human]] for context");
        assert_eq!(
            links,
            vec![NoteLink {
                target: "people/human".to_owned(),
                wiki: true
            }]
        );
    }

    #[test]
    fn a_wiki_link_with_a_label_uses_the_target() {
        let links = note_links("[[people/human|the human]]");
        assert_eq!(links[0].target, "people/human");
    }

    #[test]
    fn an_inline_link_is_extracted() {
        let links = note_links("[the spec](docs/spec.md)");
        assert_eq!(
            links,
            vec![NoteLink {
                target: "docs/spec.md".to_owned(),
                wiki: false
            }]
        );
    }

    #[test]
    fn an_angle_bracketed_destination_is_unwrapped() {
        let links = note_links("[spec](<docs/a b.md>)");
        assert_eq!(links[0].target, "docs/a b.md");
    }

    #[test]
    fn an_inline_link_with_a_title_keeps_only_the_destination() {
        let links = note_links("[spec](docs/spec.md \"the spec\")");
        assert_eq!(links[0].target, "docs/spec.md");
    }

    #[test]
    fn an_image_is_not_a_link() {
        assert!(note_links("![diagram](assets/diagram.png)").is_empty());
    }

    #[test]
    fn a_link_inside_inline_code_is_not_a_link() {
        assert!(note_links("write `[[name]]` to link").is_empty());
        assert!(note_links("write ``[[name]]`` to link").is_empty());
    }

    #[test]
    fn a_link_inside_a_fenced_block_is_not_a_link() {
        let body = "text\n```\n[[name]]\n```\nmore";
        assert!(note_links(body).is_empty());
    }

    #[test]
    fn a_tilde_fence_is_honoured_too() {
        let body = "~~~\n[[name]]\n~~~";
        assert!(note_links(body).is_empty());
    }

    #[test]
    fn a_fence_longer_than_three_backticks_closes_only_on_its_own_run() {
        let body = "````\n[[a]]\n```\n[[b]]\n````\n[[c]]";
        let links = note_links(body);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "c");
    }

    #[test]
    fn content_after_a_closed_fence_is_scanned_again() {
        let body = "```\n[[a]]\n```\n[[b]]";
        let links = note_links(body);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "b");
    }

    #[test]
    fn an_unmatched_backtick_leaves_the_rest_as_text() {
        let links = note_links("`[[name]]");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "name");
    }

    #[test]
    fn a_wiki_link_resolves_without_the_extension() {
        let existing = corpus(&["system/human.md"]);
        assert!(dangling_links("system/notes.md", "[[system/human]]", &existing).is_empty());
    }

    #[test]
    fn an_inline_link_resolves_against_its_own_directory() {
        let existing = corpus(&["system/deep/note.md"]);
        assert!(dangling_links("system/other.md", "[n](deep/note.md)", &existing).is_empty());
    }

    #[test]
    fn an_anchor_is_ignored_when_resolving() {
        let existing = corpus(&["system/human.md"]);
        assert!(dangling_links("system/a.md", "[[system/human#identity]]", &existing).is_empty());
    }

    #[test]
    fn a_missing_target_is_reported() {
        let existing = corpus(&["system/human.md"]);
        let dangling = dangling_links("system/a.md", "[[system/gone]]", &existing);
        assert_eq!(
            dangling,
            vec![DanglingLink {
                target: "system/gone".to_owned(),
                escapes: false
            }]
        );
    }

    #[test]
    fn a_target_that_escapes_the_root_is_reported_as_escaping() {
        let existing = corpus(&["system/human.md"]);
        let dangling = dangling_links("system/a.md", "[out](../../outside.md)", &existing);
        assert_eq!(dangling.len(), 1);
        assert!(dangling[0].escapes);
    }

    #[test]
    fn an_absolute_target_escapes() {
        let existing = corpus(&["system/human.md"]);
        let dangling = dangling_links("system/a.md", "[[/etc/passwd]]", &existing);
        assert!(dangling[0].escapes);
    }

    #[test]
    fn an_existing_directory_satisfies_a_link() {
        let existing = corpus(&["people/lin-kening/card.md"]);
        assert!(dangling_links("a.md", "[[people/lin-kening]]", &existing).is_empty());
    }

    #[test]
    fn an_http_url_is_not_checked() {
        let existing = corpus(&[]);
        assert!(dangling_links("a.md", "[x](https://example.com/a)", &existing).is_empty());
        assert!(dangling_links("a.md", "[x](HTTP://example.com)", &existing).is_empty());
        assert!(dangling_links("a.md", "[x](mailto:a@b.c)", &existing).is_empty());
    }

    #[test]
    fn an_empty_link_is_not_checked() {
        let existing = corpus(&[]);
        assert!(dangling_links("a.md", "[x](#section)", &existing).is_empty());
    }

    #[test]
    fn a_dot_segment_is_removed() {
        assert_eq!(normalize_path("system/./note.md"), "system/note.md");
        assert_eq!(normalize_path("./note.md"), "note.md");
    }

    #[test]
    fn a_parent_segment_climbs_one_level() {
        assert_eq!(normalize_path("system/deep/../note.md"), "system/note.md");
    }

    #[test]
    fn a_parent_segment_at_the_root_survives() {
        assert_eq!(normalize_path("../note.md"), "../note.md");
        assert_eq!(normalize_path("../../note.md"), "../../note.md");
    }

    #[test]
    fn a_link_to_the_file_itself_resolves() {
        let existing = corpus(&["note.md"]);
        assert!(dangling_links("note.md", "[[note]]", &existing).is_empty());
    }
}
