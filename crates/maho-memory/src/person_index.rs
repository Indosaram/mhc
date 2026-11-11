//! Building the routing index from the people directory, so a new fact can name someone already known.
//!
//! Spec: `docs/cleanroom/specs/memory-person-index.md`. The index is read from disk on every extraction run
//! rather than cached: a card edited between runs must be honoured, and a stale index silently files facts
//! under the wrong person.

use std::path::Path;

use crate::facts_person_routing::KnownPerson;

/// `people/human/` is never indexed; the primary human is read from `system/human.md`. Two entries for one
/// person would make every one of their names a tie.
pub const HUMAN_SLUG: &str = "human";

pub const PEOPLE_DIR: &str = "people";

pub const CARD_FILE: &str = "card.md";

/// The `Person - ` prefix is written by the card generator and is not part of anyone's name; leaving it in
/// would enter the routing index as a name nobody is ever called.
pub fn display_name_of(description: &str, fallback: &str) -> String {
    let stripped = strip_person_prefix(description)
        .map_or(description, str::trim)
        .trim();
    if stripped.is_empty() {
        fallback.to_owned()
    } else {
        stripped.to_owned()
    }
}

fn strip_person_prefix(value: &str) -> Option<&str> {
    let head = value.get(..6)?;
    if !head.eq_ignore_ascii_case("person") {
        return None;
    }
    value[6..]
        .trim_start()
        .strip_prefix('-')
        .map(str::trim_start)
}

/// Slug order, not directory order, so the index — and therefore the tie-break among equal-length names —
/// does not depend on filesystem iteration order. A person whose card is missing or unparseable is skipped
/// rather than indexed under a guessed name.
pub fn read_person_index(repo_dir: &Path) -> Vec<KnownPerson> {
    let mut index = Vec::new();
    if let Some(card) = read_card(&repo_dir.join("system").join("human.md")) {
        index.push(KnownPerson {
            slug: HUMAN_SLUG.to_owned(),
            display_name: display_name_of(&card.description, HUMAN_SLUG),
            names: card.names(HUMAN_SLUG),
        });
    }

    let people_dir = repo_dir.join(PEOPLE_DIR);
    let Ok(entries) = std::fs::read_dir(&people_dir) else {
        return index;
    };
    let mut slugs: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|slug| slug != HUMAN_SLUG)
        .collect();
    slugs.sort();
    for slug in slugs {
        let Some(card) = read_card(&people_dir.join(&slug).join(CARD_FILE)) else {
            continue;
        };
        index.push(KnownPerson {
            display_name: display_name_of(&card.description, &slug),
            names: card.names(&slug),
            slug,
        });
    }
    index
}

struct Card {
    description: String,
    aliases: Vec<String>,
}

impl Card {
    /// Display name first, then every alias, de-duplicated in that order.
    fn names(&self, fallback: &str) -> Vec<String> {
        let display = display_name_of(&self.description, fallback);
        let mut names = vec![display];
        for alias in &self.aliases {
            let alias = alias.trim();
            if !alias.is_empty() && !names.iter().any(|existing| existing == alias) {
                names.push(alias.to_owned());
            }
        }
        names
    }
}

fn read_card(path: &Path) -> Option<Card> {
    parse_card(&std::fs::read_to_string(path).ok()?)
}

/// Deliberately tolerant: a hand-written card may carry keys this reader does not know, and refusing the
/// whole card would drop the person from routing entirely.
fn parse_card(content: &str) -> Option<Card> {
    let trimmed = content.trim_start();
    let after_fence = trimmed.strip_prefix("---")?;
    let end = after_fence.find("\n---")?;
    let mut description: Option<String> = None;
    let mut aliases: Vec<String> = Vec::new();
    for line in after_fence[..end].lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "description" if !value.is_empty() => description = Some(decode(value)),
            "aliases" => aliases = parse_aliases(value),
            _ => {}
        }
    }
    Some(Card {
        description: description?,
        aliases,
    })
}

/// A plain `[a, b]` is accepted because hand-written cards use it; dropping those aliases would lose the
/// person's other names.
fn parse_aliases(value: &str) -> Vec<String> {
    if let Ok(parsed) = serde_json::from_str::<Vec<String>>(value) {
        return parsed
            .into_iter()
            .map(|alias| alias.trim().to_owned())
            .filter(|alias| !alias.is_empty())
            .collect();
    }
    let inner = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'));
    inner
        .map(|inner| {
            inner
                .split(',')
                .map(|alias| {
                    alias
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .trim()
                        .to_owned()
                })
                .filter(|alias| !alias.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn decode(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(value)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, content).expect("write");
    }

    fn card(description: &str, aliases: &str) -> String {
        format!("---\ndescription: {description}\nkind: person\naliases: {aliases}\n---\n")
    }

    #[test]
    fn a_person_prefix_is_not_part_of_the_name() {
        assert_eq!(
            display_name_of("Person - Indosaram", "fallback"),
            "Indosaram"
        );
        assert_eq!(
            display_name_of("person - Indosaram", "fallback"),
            "Indosaram"
        );
        assert_eq!(
            display_name_of("Person- Indosaram", "fallback"),
            "Indosaram"
        );
    }

    #[test]
    fn a_description_without_the_prefix_is_used_whole() {
        assert_eq!(display_name_of("Indosaram", "fallback"), "Indosaram");
    }

    #[test]
    fn an_empty_description_falls_back() {
        assert_eq!(display_name_of("Person - ", "fallback"), "fallback");
        assert_eq!(display_name_of("   ", "fallback"), "fallback");
    }

    #[test]
    fn a_name_starting_with_person_is_not_mangled() {
        assert_eq!(display_name_of("Personable", "fallback"), "Personable");
        assert_eq!(display_name_of("Persona", "fallback"), "Persona");
    }

    #[test]
    fn the_human_card_is_indexed_first_from_the_system_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "system/human.md",
            &card("Person - Indosaram", "[\"Indo\"]"),
        );
        write(
            dir.path(),
            "people/other/card.md",
            &card("Person - Other", "[]"),
        );
        let index = read_person_index(dir.path());
        assert_eq!(index[0].slug, HUMAN_SLUG);
        assert_eq!(index[0].display_name, "Indosaram");
        assert_eq!(
            index[0].names,
            vec!["Indosaram".to_owned(), "Indo".to_owned()]
        );
        assert_eq!(index[1].slug, "other");
    }

    #[test]
    fn people_are_indexed_in_slug_order_not_directory_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        for slug in ["zeta", "alpha", "mu"] {
            write(
                dir.path(),
                &format!("people/{slug}/card.md"),
                &card(slug, "[]"),
            );
        }
        let slugs: Vec<String> = read_person_index(dir.path())
            .into_iter()
            .map(|person| person.slug)
            .collect();
        assert_eq!(slugs, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn a_people_directory_named_human_does_not_duplicate_the_primary() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "system/human.md", &card("Person - Me", "[]"));
        write(
            dir.path(),
            "people/human/card.md",
            &card("Person - Me", "[]"),
        );
        assert_eq!(read_person_index(dir.path()).len(), 1);
    }

    #[test]
    fn a_person_without_a_card_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("people/nocard")).expect("mkdir");
        write(
            dir.path(),
            "people/has/card.md",
            &card("Person - Has", "[]"),
        );
        let slugs: Vec<String> = read_person_index(dir.path())
            .into_iter()
            .map(|person| person.slug)
            .collect();
        assert_eq!(slugs, vec!["has"]);
    }

    #[test]
    fn a_card_without_a_description_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "people/nodesc/card.md",
            "---\nkind: person\n---\n",
        );
        assert!(read_person_index(dir.path()).is_empty());
    }

    #[test]
    fn a_missing_people_directory_yields_only_the_human() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "system/human.md", &card("Person - Me", "[]"));
        assert_eq!(read_person_index(dir.path()).len(), 1);
    }

    #[test]
    fn an_empty_repository_yields_an_empty_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read_person_index(dir.path()).is_empty());
    }

    #[test]
    fn aliases_and_display_name_are_all_names_without_duplicates() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "people/a/card.md",
            &card("Person - Name", "[\"Name\", \"Alias\", \"\"]"),
        );
        let index = read_person_index(dir.path());
        assert_eq!(index[0].names, vec!["Name".to_owned(), "Alias".to_owned()]);
    }

    #[test]
    fn a_hand_written_alias_list_is_accepted() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "people/a/card.md",
            &card("Person - Name", "[Alias, \"Other\"]"),
        );
        let index = read_person_index(dir.path());
        assert_eq!(
            index[0].names,
            vec!["Name".to_owned(), "Alias".to_owned(), "Other".to_owned()]
        );
    }

    #[test]
    fn a_quoted_alias_array_is_not_treated_as_aliases() {
        assert_eq!(parse_aliases("\"[\\\"A\\\"]\""), Vec::<String>::new());
        assert_eq!(parse_aliases("[\"A\"]"), vec!["A".to_owned()]);
    }

    #[test]
    fn an_unparseable_alias_value_drops_aliases_but_keeps_the_person() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "people/a/card.md",
            &card("Person - Name", "not a list"),
        );
        let index = read_person_index(dir.path());
        assert_eq!(index[0].names, vec!["Name".to_owned()]);
    }

    #[test]
    fn the_index_feeds_the_router() {
        use crate::facts_person_routing::{PersonClaim, resolve_person_slug};
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "system/human.md",
            &card("Person - Indosaram", "[\"Indo\"]"),
        );
        let index = read_person_index(dir.path());
        let claim = PersonClaim {
            name: "Indo".to_owned(),
            aliases: Vec::new(),
        };
        assert_eq!(
            resolve_person_slug(&index, &claim, None),
            Some(HUMAN_SLUG.to_owned())
        );
    }

    #[test]
    fn a_unicode_slug_is_kept_verbatim() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "people/윤인도/card.md",
            &card("Person - 윤인도", "[]"),
        );
        assert_eq!(read_person_index(dir.path())[0].slug, "윤인도");
    }
}
