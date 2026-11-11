//! Rendering a monthly facts note: append-only, one bullet per fact, existing content preserved.
//!
//! Spec: `docs/cleanroom/specs/memory-facts-note.md`. Rendering is a pure function of (existing file,
//! records): the applier writes what was planned, so the same input must produce the same bytes or the
//! recovery envelope's hashes stop describing the file.

use crate::normalize::{
    FrontmatterError, MemoryFrontmatter, ParsedMemoryFile, parse_memory_file, render_memory_file,
};

/// First seven characters (`YYYY-MM`), so every fact in a month shares one file. A malformed date still
/// lands somewhere rather than failing the whole batch.
pub fn facts_note_path(date: &str) -> String {
    let month: String = date.chars().take(7).collect();
    format!("notes/facts/{month}.md")
}

/// Built from the path rather than the date so an existing note's description is never touched; repairing a
/// description is a separate operation with its own preflight.
pub fn default_description(relative_path: &str) -> String {
    let stem = relative_path
        .rsplit('/')
        .next()
        .unwrap_or(relative_path)
        .trim_end_matches(".md");
    format!("Explicit facts for {stem}")
}

/// Appends to `existing`, never rewrites: earlier facts may come from another session and must survive.
pub fn render_facts_note(
    existing: Option<&str>,
    relative_path: &str,
    records: &[(String, String)],
) -> Result<String, FrontmatterError> {
    let (frontmatter, body) = match existing {
        Some(source) => {
            let ParsedMemoryFile { frontmatter, body } = parse_memory_file(source)?;
            (frontmatter, body)
        }
        None => (
            MemoryFrontmatter {
                description: default_description(relative_path),
                read_only: None,
                kind: None,
                aliases: None,
                extra: None,
            },
            String::new(),
        ),
    };
    // Without this separator the first new bullet joins the last old line.
    let separator = if body.is_empty() || body.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let mut bullets = String::new();
    for (index, (date, text)) in records.iter().enumerate() {
        if index > 0 {
            bullets.push('\n');
        }
        bullets.push_str(&format!("- [{date}] {text}"));
    }
    let body = format!("{body}{separator}{bullets}\n");
    render_memory_file(&frontmatter, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records() -> Vec<(String, String)> {
        vec![
            ("2026-11-01".to_owned(), "first fact".to_owned()),
            ("2026-11-02".to_owned(), "second fact".to_owned()),
        ]
    }

    fn render(existing: Option<&str>, records: &[(String, String)]) -> String {
        render_facts_note(existing, "notes/facts/2026-11.md", records).expect("renders")
    }

    #[test]
    fn a_monthly_path_uses_the_first_seven_characters() {
        assert_eq!(facts_note_path("2026-11-09"), "notes/facts/2026-11.md");
        assert_eq!(facts_note_path("2026-11"), "notes/facts/2026-11.md");
    }

    #[test]
    fn a_malformed_date_still_produces_a_path() {
        assert_eq!(facts_note_path(""), "notes/facts/.md");
        assert_eq!(facts_note_path("2026"), "notes/facts/2026.md");
    }

    #[test]
    fn the_default_description_names_the_month() {
        assert_eq!(
            default_description("notes/facts/2026-11.md"),
            "Explicit facts for 2026-11"
        );
    }

    #[test]
    fn a_new_note_gets_frontmatter_and_one_bullet_per_fact() {
        let rendered = render(None, &records());
        assert!(
            rendered.starts_with("---\ndescription: Explicit facts for 2026-11\n---\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- [2026-11-01] first fact\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- [2026-11-02] second fact\n"),
            "{rendered}"
        );
        assert!(rendered.ends_with('\n'));
    }

    #[test]
    fn appending_keeps_every_existing_fact() {
        let existing =
            "---\ndescription: Explicit facts for 2026-11\n---\n- [2026-10-31] older fact\n";
        let rendered = render(
            Some(existing),
            &[("2026-11-01".to_owned(), "new fact".to_owned())],
        );
        let older = rendered.find("older fact").expect("older");
        let newer = rendered.find("new fact").expect("newer");
        assert!(
            older < newer,
            "new facts go after existing ones: {rendered}"
        );
    }

    #[test]
    fn appending_does_not_join_the_last_line() {
        let existing = "---\ndescription: d\n---\n- [2026-10-31] older fact";
        let rendered = render(
            Some(existing),
            &[("2026-11-01".to_owned(), "new fact".to_owned())],
        );
        assert!(
            rendered.contains("older fact\n- [2026-11-01] new fact"),
            "{rendered}"
        );
    }

    #[test]
    fn an_existing_description_is_never_rewritten() {
        let existing = "---\ndescription: my own words\n---\n- [2026-10-31] older fact\n";
        let rendered = render(
            Some(existing),
            &[("2026-11-01".to_owned(), "new fact".to_owned())],
        );
        assert!(
            rendered.starts_with("---\ndescription: my own words\n---\n"),
            "{rendered}"
        );
        assert!(!rendered.contains("Explicit facts for"), "{rendered}");
    }

    #[test]
    fn provenance_in_the_fact_text_is_carried_verbatim() {
        let text = "likes tea (source: user said \"i like tea\")";
        let rendered = render(None, &[("2026-11-01".to_owned(), text.to_owned())]);
        assert!(rendered.contains(text), "{rendered}");
    }

    #[test]
    fn rendering_the_same_input_twice_is_byte_identical() {
        assert_eq!(render(None, &records()), render(None, &records()));
    }

    #[test]
    fn no_records_still_produces_a_valid_note() {
        let rendered = render(None, &[]);
        assert!(rendered.starts_with("---\ndescription:"), "{rendered}");
        assert!(!rendered.contains("- ["), "{rendered}");
    }

    #[test]
    fn an_empty_description_is_refused() {
        let existing = "---\ndescription:\n---\n- [2026-10-31] a fact\n";
        assert!(render_facts_note(Some(existing), "notes/facts/2026-11.md", &records()).is_err());
    }

    #[test]
    fn an_existing_read_only_flag_survives() {
        let existing = "---\ndescription: kept\nread_only: true\n---\n- [2026-10-31] a fact\n";
        let rendered = render(Some(existing), &records());
        assert!(rendered.contains("read_only: true"), "{rendered}");
    }
}
